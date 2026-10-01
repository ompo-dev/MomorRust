//! Windows WebView2 host used by Momor's browser panel.
//!
//! The WebView2 controller is a child of the Momor window. This keeps the
//! browser content native and interactive while Momor owns the surrounding UI.

#![cfg(target_os = "windows")]

use anyhow::{Context as _, Result, anyhow};
use std::{cell::RefCell, path::Path, rc::Rc};
use webview2_com::{
    CoTaskMemPWSTR, CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, SourceChangedEventHandler,
    Microsoft::Web::WebView2::Win32::*,
};
use windows_062::{
    Win32::{
        Foundation::{HWND, RECT},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
        UI::Input::KeyboardAndMouse::SetFocus,
    },
    core::{BOOL, Interface, PCWSTR},
};

pub struct NativeWebView {
    parent: HWND,
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
    source_changed_token: i64,
}

pub fn debugging_port() -> u16 {
    std::env::var("MOMOR_BROWSER_CDP_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|port: &u16| *port != 0)
        .unwrap_or(9224)
}

impl NativeWebView {
    pub fn create(
        parent: HWND,
        profile_dir: &Path,
        mut on_url_changed: impl FnMut(String) + 'static,
        on_ready: impl FnOnce(Result<Self>) + 'static,
    ) -> Result<()> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .context("falha ao inicializar COM para o WebView2")?;

        let on_ready = Rc::new(RefCell::new(Some(
            Box::new(on_ready) as Box<dyn FnOnce(Result<Self>)>
        )));
        let profile = CoTaskMemPWSTR::from(profile_dir.to_string_lossy().as_ref());
        let environment_options = CoreWebView2EnvironmentOptions::default();
        unsafe {
            environment_options.set_additional_browser_arguments(format!(
                "--remote-debugging-port={} --remote-allow-origins=*",
                debugging_port()
            ));
        }
        let environment_ready = on_ready.clone();
        let environment_handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
            move |error_code, environment| {
                if let Err(error) = error_code {
                    complete(
                        &environment_ready,
                        Err(anyhow!("falha ao criar o ambiente WebView2: {error}")),
                    );
                    return Ok(());
                }
                let Some(environment) = environment else {
                    complete(
                        &environment_ready,
                        Err(anyhow!("WebView2 não retornou o ambiente")),
                    );
                    return Ok(());
                };

                let controller_ready = environment_ready.clone();
                let controller_handler = CreateCoreWebView2ControllerCompletedHandler::create(
                    Box::new(move |error_code, controller| {
                        if let Err(error) = error_code {
                            complete(
                                &controller_ready,
                                Err(anyhow!("falha ao criar o controlador WebView2: {error}")),
                            );
                            return Ok(());
                        }
                        let Some(controller) = controller else {
                            complete(
                                &controller_ready,
                                Err(anyhow!("WebView2 não retornou o controlador")),
                            );
                            return Ok(());
                        };

                        let result = (|| {
                            let webview = unsafe { controller.CoreWebView2() }
                                .context("WebView2 não retornou a página")?;
                            let source_changed_handler = SourceChangedEventHandler::create(
                                Box::new(move |sender, _args| {
                                    let Some(sender) = sender else {
                                        return Ok(());
                                    };
                                    let mut source_pointer = windows_062::core::PWSTR::null();
                                    unsafe { sender.Source(&mut source_pointer)? };
                                    let source = CoTaskMemPWSTR::from(source_pointer);
                                    let url = source.to_string();
                                    if !url.is_empty() {
                                        on_url_changed(url);
                                    }
                                    Ok(())
                                }),
                            );
                            let mut source_changed_token = 0;
                            unsafe {
                                webview
                                    .add_SourceChanged(
                                        &source_changed_handler,
                                        &mut source_changed_token,
                                    )
                                    .context("não foi possível observar mudanças de URL")?;
                                let controller2: ICoreWebView2Controller2 = controller
                                    .cast()
                                    .context("WebView2 não expôs o controlador visual")?;
                                controller2
                                    .SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                                        A: 255,
                                        R: 255,
                                        G: 255,
                                        B: 255,
                                    })
                                    .context("não foi possível definir o fundo do WebView2")?;
                                webview
                                    .Settings()?
                                    .SetAreDevToolsEnabled(true)
                                    .context("não foi possível habilitar o DevTools")?;
                                controller
                                    .SetIsVisible(true)
                                    .context("não foi possível exibir o WebView2")?;
                            }
                            Ok(Self {
                                parent,
                                controller,
                                webview,
                                source_changed_token,
                            })
                        })();
                        complete(&controller_ready, result);
                        Ok(())
                    }),
                );
                unsafe { environment.CreateCoreWebView2Controller(parent, &controller_handler) }
            },
        ));

        let environment_options: ICoreWebView2EnvironmentOptions = environment_options.into();

        unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                PCWSTR::null(),
                *profile.as_ref().as_pcwstr(),
                Some(&environment_options),
                &environment_handler,
            )
            .map_err(webview2_com::Error::WindowsError)
            .context("falha ao iniciar o ambiente WebView2")?;
        }
        Ok(())
    }

    pub fn set_bounds(&self, left: i32, top: i32, width: i32, height: i32) -> Result<()> {
        unsafe {
            self.controller
                .SetBounds(RECT {
                    left,
                    top,
                    right: left + width.max(1),
                    bottom: top + height.max(1),
                })
                .context("não foi possível redimensionar o WebView2")?;
        }
        Ok(())
    }

    pub fn set_visible(&self, visible: bool) -> Result<()> {
        unsafe {
            self.controller
                .SetIsVisible(visible)
                .context("não foi possível alterar a visibilidade do WebView2")?;
            if !visible {
                // WebView2 is a native child HWND. Hiding its controller does not
                // automatically return keyboard focus to GPUI.
                SetFocus(Some(self.parent)).context("não foi possível devolver o foco ao Momor")?;
            }
        }
        Ok(())
    }

    pub fn navigate(&self, url: &str) -> Result<()> {
        let url = CoTaskMemPWSTR::from(url);
        unsafe {
            self.webview
                .Navigate(*url.as_ref().as_pcwstr())
                .context("não foi possível navegar no WebView2")?;
        }
        Ok(())
    }

    pub fn reload(&self) -> Result<()> {
        unsafe {
            self.webview
                .Reload()
                .context("não foi possível atualizar a página")?;
        }
        Ok(())
    }

    pub fn go_back(&self) -> Result<bool> {
        unsafe {
            let mut can_go_back = BOOL(0);
            self.webview
                .CanGoBack(&mut can_go_back)
                .context("falha ao consultar histórico")?;
            if !can_go_back.as_bool() {
                return Ok(false);
            }
            self.webview
                .GoBack()
                .context("não foi possível voltar no histórico")?;
        }
        Ok(true)
    }

    pub fn go_forward(&self) -> Result<bool> {
        unsafe {
            let mut can_go_forward = BOOL(0);
            self.webview
                .CanGoForward(&mut can_go_forward)
                .context("falha ao consultar histórico")?;
            if !can_go_forward.as_bool() {
                return Ok(false);
            }
            self.webview
                .GoForward()
                .context("não foi possível avançar no histórico")?;
        }
        Ok(true)
    }

    pub fn open_devtools(&self) -> Result<()> {
        unsafe {
            self.webview
                .OpenDevToolsWindow()
                .context("não foi possível abrir o DevTools")?;
        }
        Ok(())
    }
}

fn complete(
    callback: &Rc<RefCell<Option<Box<dyn FnOnce(Result<NativeWebView>)>>>>,
    result: Result<NativeWebView>,
) {
    let callback = callback.borrow_mut().take();
    if let Some(callback) = callback {
        callback(result);
    }
}

impl Drop for NativeWebView {
    fn drop(&mut self) {
        unsafe {
            let _ = self.webview.remove_SourceChanged(self.source_changed_token);
            let _ = self.controller.Close();
        }
    }
}
