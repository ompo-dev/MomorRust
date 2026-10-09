//! Windows WebView2 host used by Momor's browser panel.
//!
//! The WebView2 controller is a child of the Momor window. This keeps the
//! browser content native and interactive while Momor owns the surrounding UI.

#![cfg(target_os = "windows")]

use anyhow::{Context as _, Result, anyhow};
use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc,
};
use webview2_com::{
    CoTaskMemPWSTR, CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, DocumentTitleChangedEventHandler,
    Microsoft::Web::WebView2::Win32::*, SourceChangedEventHandler,
};
use windows_062::{
    Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::{CombineRgn, CreateRectRgn, DeleteObject, HRGN, RGN_DIFF, SetWindowRgn},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
        UI::{
            Input::KeyboardAndMouse::{GetFocus, SetFocus},
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, IsChild, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE,
                SWP_NOZORDER, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WS_CHILD, WS_CLIPCHILDREN,
                WS_CLIPSIBLINGS,
            },
        },
    },
    core::{BOOL, Interface, PCWSTR, w},
};

pub struct NativeWebView {
    parent: HWND,
    host: Rc<ChildHost>,
    visible: Cell<bool>,
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
    source_changed_token: i64,
    title_changed_token: i64,
}

struct ChildHost {
    window: HWND,
    bounds: Cell<(i32, i32, i32, i32)>,
    occlusions: RefCell<Vec<(i32, i32, i32, i32)>>,
}

impl Drop for ChildHost {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyWindow(self.window) } {
            tracing::debug!("failed to destroy browser host: {error}");
        }
    }
}

struct OwnedRegion(Option<HRGN>);

impl OwnedRegion {
    fn rectangle(left: i32, top: i32, right: i32, bottom: i32) -> Result<Self> {
        let region = unsafe { CreateRectRgn(left, top, right, bottom) };
        anyhow::ensure!(
            !region.is_invalid(),
            "failed to allocate browser clipping region"
        );
        Ok(Self(Some(region)))
    }
}

impl Drop for OwnedRegion {
    fn drop(&mut self) {
        if let Some(region) = self.0.take()
            && !unsafe { DeleteObject(region.into()) }.as_bool()
        {
            tracing::error!("failed to release browser clipping region");
        }
    }
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
        mut on_title_changed: impl FnMut(String) + 'static,
        on_ready: impl FnOnce(Result<Self>) + 'static,
    ) -> Result<()> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .context("falha ao inicializar COM para o WebView2")?;

        // A dedicated child host gives us HWND clipping without changing the
        // WebView document's visibility or its media lifecycle for GPUI menus.
        let host = Rc::new(ChildHost {
            window: unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Momor Browser"),
                    WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                    0,
                    0,
                    1,
                    1,
                    Some(parent),
                    None,
                    None,
                    None,
                )
            }
            .context("failed to create browser child host")?,
            bounds: Cell::new((0, 0, 1, 1)),
            occlusions: RefCell::new(Vec::new()),
        });

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
                let controller_parent = host.window;
                let host = host.clone();
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
                            let title_changed_handler = DocumentTitleChangedEventHandler::create(
                                Box::new(move |sender, _args| {
                                    let Some(sender) = sender else {
                                        return Ok(());
                                    };
                                    let mut title_pointer = windows_062::core::PWSTR::null();
                                    unsafe { sender.DocumentTitle(&mut title_pointer)? };
                                    let title = CoTaskMemPWSTR::from(title_pointer).to_string();
                                    on_title_changed(title);
                                    Ok(())
                                }),
                            );
                            let mut source_changed_token = 0;
                            let mut title_changed_token = 0;
                            unsafe {
                                webview
                                    .add_SourceChanged(
                                        &source_changed_handler,
                                        &mut source_changed_token,
                                    )
                                    .context("não foi possível observar mudanças de URL")?;
                                webview
                                    .add_DocumentTitleChanged(
                                        &title_changed_handler,
                                        &mut title_changed_token,
                                    )
                                    .context("não foi possível observar mudanças de título")?;
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
                                controller.SetIsVisible(false).context(
                                    "não foi possível inicializar a visibilidade do WebView2",
                                )?;
                            }
                            Ok(Self {
                                parent,
                                host: host.clone(),
                                visible: Cell::new(false),
                                controller,
                                webview,
                                source_changed_token,
                                title_changed_token,
                            })
                        })();
                        complete(&controller_ready, result);
                        Ok(())
                    }),
                );
                unsafe {
                    environment.CreateCoreWebView2Controller(controller_parent, &controller_handler)
                }
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
        let bounds = (left, top, width.max(1), height.max(1));
        unsafe {
            SetWindowPos(
                self.host.window,
                None,
                left,
                top,
                bounds.2,
                bounds.3,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
            .context("failed to position browser host")?;
            self.controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: bounds.2,
                    bottom: bounds.3,
                })
                .context("não foi possível redimensionar o WebView2")?;
        }
        self.host.bounds.set(bounds);
        self.apply_occlusions(&self.host.occlusions.borrow())?;
        Ok(())
    }

    pub fn set_occlusions(&self, rectangles: &[(i32, i32, i32, i32)]) -> Result<()> {
        if self.host.occlusions.borrow().as_slice() == rectangles {
            return Ok(());
        }
        self.apply_occlusions(rectangles)?;
        *self.host.occlusions.borrow_mut() = rectangles.to_vec();
        Ok(())
    }

    fn apply_occlusions(&self, rectangles: &[(i32, i32, i32, i32)]) -> Result<()> {
        let (left, top, width, height) = self.host.bounds.get();
        if rectangles.is_empty() {
            anyhow::ensure!(
                unsafe { SetWindowRgn(self.host.window, None, true) } != 0,
                "failed to clear browser clipping region"
            );
            return Ok(());
        }
        let mut region = OwnedRegion::rectangle(0, 0, width, height)?;
        for &(x1, y1, x2, y2) in rectangles {
            let cutout = OwnedRegion::rectangle(x1 - left, y1 - top, x2 - left, y2 - top)?;
            anyhow::ensure!(
                unsafe { CombineRgn(region.0, region.0, cutout.0, RGN_DIFF) }.0 != 0,
                "failed to combine browser clipping regions"
            );
        }
        anyhow::ensure!(
            unsafe { SetWindowRgn(self.host.window, region.0, true) } != 0,
            "failed to apply browser clipping region"
        );
        // SetWindowRgn owns the region after success.
        region.0.take();
        Ok(())
    }

    pub fn set_visible(&self, visible: bool) -> Result<()> {
        if self.visible.get() == visible {
            return Ok(());
        }
        unsafe {
            let focus = GetFocus();
            let browser_focused =
                focus == self.host.window || IsChild(self.host.window, focus).as_bool();
            self.controller
                .SetIsVisible(visible)
                .context("não foi possível alterar a visibilidade do WebView2")?;
            let _previous_visibility =
                ShowWindow(self.host.window, if visible { SW_SHOWNA } else { SW_HIDE });
            if !visible && browser_focused {
                // WebView2 is a native child HWND. Hiding its controller does not
                // automatically return keyboard focus to GPUI.
                SetFocus(Some(self.parent)).context("não foi possível devolver o foco ao Momor")?;
            }
        }
        self.visible.set(visible);
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
            if let Err(error) = self.webview.remove_SourceChanged(self.source_changed_token) {
                tracing::debug!("failed to remove browser source listener: {error}");
            }
            if let Err(error) = self
                .webview
                .remove_DocumentTitleChanged(self.title_changed_token)
            {
                tracing::debug!("failed to remove browser title listener: {error}");
            }
            if let Err(error) = self.controller.Close() {
                tracing::debug!("failed to close browser controller: {error}");
            }
        }
    }
}
