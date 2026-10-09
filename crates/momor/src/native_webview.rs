//! Windows WebView2 host used by Momor's browser panel.
//!
//! The WebView2 controller is a child of the Momor window. This keeps the
//! browser content native and interactive while Momor owns the surrounding UI.

#![cfg(target_os = "windows")]

use anyhow::{Context as _, Result, anyhow};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    path::Path,
    rc::Rc,
};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DevToolsProtocolEventReceivedEventHandler, DocumentTitleChangedEventHandler,
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
    frame_activity: Rc<FrameActivity>,
    frame_receivers: Vec<(ICoreWebView2DevToolsProtocolEventReceiver, i64)>,
}

struct FrameActivity {
    enabled: Cell<bool>,
    sessions: RefCell<HashSet<String>>,
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
        keep_pages_active: bool,
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
                                frame_activity: Rc::new(FrameActivity {
                                    enabled: Cell::new(keep_pages_active),
                                    sessions: RefCell::new(HashSet::new()),
                                }),
                                frame_receivers: Vec::new(),
                            })
                        })();
                        match result {
                            Ok(mut webview) => {
                                if let Err(error) = webview.observe_iframe_activity() {
                                    complete(&controller_ready, Err(error));
                                    return Ok(());
                                }
                                let core = webview.webview.clone();
                                let ready = controller_ready.clone();
                                // Apply the renderer policy before the caller can navigate to a site.
                                if let Err(error) = apply_page_activity_policy(
                                    &core,
                                    None,
                                    keep_pages_active,
                                    move |result| {
                                        if let Err(error) = result {
                                            complete(&ready, Err(error));
                                            return;
                                        }
                                        let core = webview.webview.clone();
                                        let completed = ready.clone();
                                        if let Err(error) =
                                            auto_attach_iframes(&core, None, move |result| {
                                                complete(&completed, result.map(|()| webview));
                                            })
                                        {
                                            complete(&ready, Err(error));
                                        }
                                    },
                                ) {
                                    complete(&controller_ready, Err(error));
                                }
                            }
                            Err(error) => complete(&controller_ready, Err(error)),
                        }
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

    pub fn set_keep_pages_active(
        &self,
        enabled: bool,
        on_complete: impl FnOnce(Result<()>) + 'static,
    ) -> Result<()> {
        self.frame_activity.enabled.set(enabled);
        let sessions = self
            .frame_activity
            .sessions
            .borrow()
            .iter()
            .cloned()
            .collect();
        let core = self.webview.clone();
        let completion: PolicyCompletion = Rc::new(RefCell::new(Some(Box::new(on_complete))));
        let pending = completion.clone();
        let result = apply_page_activity_policy(&self.webview, None, enabled, move |result| {
            if let Err(error) = result {
                finish_policy(&pending, Err(error));
            } else {
                update_iframe_policies(
                    core,
                    Rc::new(RefCell::new(sessions)),
                    enabled,
                    Rc::new(RefCell::new(None)),
                    pending,
                );
            }
        });
        if result.is_err() {
            completion.borrow_mut().take();
        }
        result
    }

    fn observe_iframe_activity(&mut self) -> Result<()> {
        for (event_name, attached) in [
            ("Target.attachedToTarget", true),
            ("Target.detachedFromTarget", false),
        ] {
            let core = self.webview.clone();
            let state = self.frame_activity.clone();
            let handler =
                DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let result = (|| {
                        let mut json = windows_062::core::PWSTR::null();
                        unsafe { args.ParameterObjectAsJson(&mut json) }?;
                        let parameters: serde_json::Value =
                            serde_json::from_str(&CoTaskMemPWSTR::from(json).to_string())?;
                        let session = parameters
                            .get("sessionId")
                            .and_then(|value| value.as_str())
                            .context("Iframe event without session ID")?
                            .to_string();
                        if attached {
                            state.sessions.borrow_mut().insert(session.clone());
                            prepare_iframe(core.clone(), session, state.clone());
                        } else {
                            state.sessions.borrow_mut().remove(&session);
                        }
                        anyhow::Ok(())
                    })();
                    if let Err(error) = result {
                        tracing::error!("Failed to track browser iframe activity: {error:#}");
                    }
                    Ok(())
                }));
            let name = CoTaskMemPWSTR::from(event_name);
            let receiver = unsafe {
                self.webview
                    .GetDevToolsProtocolEventReceiver(*name.as_ref().as_pcwstr())
            }?;
            let mut token = 0;
            unsafe { receiver.add_DevToolsProtocolEventReceived(&handler, &mut token) }?;
            self.frame_receivers.push((receiver, token));
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

fn apply_page_activity_policy(
    webview: &ICoreWebView2,
    session: Option<&str>,
    enabled: bool,
    on_complete: impl FnOnce(Result<()>) + 'static,
) -> Result<()> {
    call_protocol(
        webview,
        session,
        "Emulation.setFocusEmulationEnabled",
        serde_json::json!({ "enabled": enabled }),
        on_complete,
    )
}

fn call_protocol(
    webview: &ICoreWebView2,
    session: Option<&str>,
    method: &str,
    parameters: serde_json::Value,
    on_complete: impl FnOnce(Result<()>) + 'static,
) -> Result<()> {
    let parameters = CoTaskMemPWSTR::from(parameters.to_string().as_str());
    let method = CoTaskMemPWSTR::from(method);
    let handler =
        CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |status, json| {
            let result = (|| {
                status.context(
                    "O WebView2 nao conseguiu aplicar a politica de atividade da pagina",
                )?;
                let result: serde_json::Value = serde_json::from_str(&json)?;
                anyhow::ensure!(
                    result.get("error").is_none(),
                    "Falha na politica de atividade: {result}"
                );
                Ok(())
            })();
            on_complete(result);
            Ok(())
        }));
    let result = unsafe {
        if let Some(session) = session {
            let core: ICoreWebView2_11 = webview.cast()?;
            let session = CoTaskMemPWSTR::from(session);
            core.CallDevToolsProtocolMethodForSession(
                *session.as_ref().as_pcwstr(),
                *method.as_ref().as_pcwstr(),
                *parameters.as_ref().as_pcwstr(),
                &handler,
            )
        } else {
            webview.CallDevToolsProtocolMethod(
                *method.as_ref().as_pcwstr(),
                *parameters.as_ref().as_pcwstr(),
                &handler,
            )
        }
    };
    result.context("Nao foi possivel solicitar a politica de atividade da pagina")?;
    Ok(())
}

fn auto_attach_iframes(
    webview: &ICoreWebView2,
    session: Option<&str>,
    on_complete: impl FnOnce(Result<()>) + 'static,
) -> Result<()> {
    // Pause only new iframe targets until their native policy is set; never attach to workers.
    call_protocol(
        webview,
        session,
        "Target.setAutoAttach",
        serde_json::json!({
            "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true,
            "filter": [{ "type": "iframe", "exclude": false }, { "exclude": true }],
        }),
        on_complete,
    )
}

type PolicyCompletion = Rc<RefCell<Option<Box<dyn FnOnce(Result<()>)>>>>;

fn finish_policy(completion: &PolicyCompletion, result: Result<()>) {
    let callback = completion.borrow_mut().take();
    if let Some(callback) = callback {
        callback(result);
    }
}

fn update_iframe_policies(
    webview: ICoreWebView2,
    sessions: Rc<RefCell<Vec<String>>>,
    enabled: bool,
    first_error: Rc<RefCell<Option<anyhow::Error>>>,
    completion: PolicyCompletion,
) {
    let session = sessions.borrow_mut().pop();
    let Some(session) = session else {
        let result = match first_error.borrow_mut().take() {
            Some(error) => Err(error),
            None => Ok(()),
        };
        finish_policy(&completion, result);
        return;
    };
    let core = webview.clone();
    let pending = completion.clone();
    let remaining = sessions.clone();
    let errors = first_error.clone();
    if let Err(error) =
        apply_page_activity_policy(&webview, Some(&session), enabled, move |result| {
            if let Err(error) = result {
                tracing::debug!("Failed to update iframe activity: {error:#}");
                errors.borrow_mut().get_or_insert(error);
            }
            update_iframe_policies(core, remaining, enabled, errors, pending);
        })
    {
        tracing::debug!("Failed to request iframe activity: {error:#}");
        first_error.borrow_mut().get_or_insert(error);
        update_iframe_policies(webview, sessions, enabled, first_error, completion);
    }
}

fn resume_iframe(webview: &ICoreWebView2, session: &str) {
    if let Err(error) = call_protocol(
        webview,
        Some(session),
        "Runtime.runIfWaitingForDebugger",
        serde_json::json!({}),
        |result| {
            if let Err(error) = result {
                tracing::debug!("Failed to resume browser iframe: {error:#}");
            }
        },
    ) {
        tracing::debug!("Failed to request iframe resume: {error:#}");
    }
}

fn prepare_iframe(webview: ICoreWebView2, session: String, state: Rc<FrameActivity>) {
    let enabled = state.enabled.get();
    let core = webview.clone();
    let frame = session.clone();
    if let Err(error) =
        apply_page_activity_policy(&webview, Some(&session), enabled, move |result| {
            if let Err(error) = result {
                tracing::error!("Failed to configure iframe activity: {error:#}");
            }
            if state.enabled.get() != enabled {
                prepare_iframe(core, frame, state);
                return;
            }
            let nested = core.clone();
            let nested_frame = frame.clone();
            if let Err(error) = auto_attach_iframes(&core, Some(&frame), move |result| {
                if let Err(error) = result {
                    tracing::error!("Failed to track nested browser iframes: {error:#}");
                }
                resume_iframe(&nested, &nested_frame);
            }) {
                tracing::error!("Failed to request nested iframe tracking: {error:#}");
                resume_iframe(&core, &frame);
            }
        })
    {
        tracing::error!("Failed to request iframe activity: {error:#}");
        resume_iframe(&webview, &session);
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
            for (receiver, token) in &self.frame_receivers {
                if let Err(error) = receiver.remove_DevToolsProtocolEventReceived(*token) {
                    tracing::debug!("Failed to remove iframe activity listener: {error}");
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use webview2_com::ExecuteScriptCompletedHandler;
    use windows_062::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WS_OVERLAPPED,
    };

    fn wait_for<T>(result: &Rc<RefCell<Option<T>>>) -> Result<T> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(value) = result.borrow_mut().take() {
                return Ok(value);
            }
            anyhow::ensure!(Instant::now() < deadline, "WebView2 test timed out");
            let mut message = MSG::default();
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                unsafe {
                    let _translated = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn evaluate(view: &NativeWebView, source: &str) -> Result<serde_json::Value> {
        let result = Rc::new(RefCell::new(None));
        let done = result.clone();
        let callback = ExecuteScriptCompletedHandler::create(Box::new(move |status, json| {
            *done.borrow_mut() = Some(status.map(|()| json));
            Ok(())
        }));
        let source = CoTaskMemPWSTR::from(source);
        unsafe {
            view.webview
                .ExecuteScript(*source.as_ref().as_pcwstr(), &callback)
        }?;
        Ok(serde_json::from_str(&wait_for(&result)??)?)
    }

    fn create_view(parent: HWND, profile: &Path, enabled: bool) -> Result<NativeWebView> {
        let result = Rc::new(RefCell::new(None));
        let done = result.clone();
        NativeWebView::create(
            parent,
            profile,
            enabled,
            |_| {},
            |_| {},
            move |view| {
                *done.borrow_mut() = Some(view);
            },
        )?;
        wait_for(&result)?
    }

    fn set_policy(view: &NativeWebView, enabled: bool) -> Result<()> {
        let result = Rc::new(RefCell::new(None));
        let done = result.clone();
        view.set_keep_pages_active(enabled, move |status| *done.borrow_mut() = Some(status))?;
        wait_for(&result)?
    }

    fn load_fixture(view: &NativeWebView, marker: &str) -> Result<()> {
        let html = format!(
            r#"<!doctype html><input id="first"><input id="second">
            <iframe sandbox="allow-scripts" srcdoc="
            <input id=target>
            <script>
            let departures = 0;
            addEventListener('blur', () => departures++);
            document.addEventListener('visibilitychange', () => departures++);
            addEventListener('message', event => {{
                if (event.data === 'focus') {{ target.focus(); departures = 0; }}
                parent.postMessage({{
                    child: true, hidden: document.hidden, visibility: document.visibilityState,
                    focused: document.hasFocus(), departures
                }}, '*');
            }});
            </script>"></iframe>
            <script>
            window.marker = '{marker}';
            window.initial = {{ hidden: document.hidden, visibility: document.visibilityState,
                focused: document.hasFocus() }};
            window.departures = 0;
            window.fieldBlurs = 0;
            document.addEventListener('visibilitychange', () => window.departures++);
            window.addEventListener('blur', () => window.departures++);
            first.addEventListener('blur', () => window.fieldBlurs++);
            first.focus();
            addEventListener('message', event => {{
                if (event.data.child) window.childState = event.data;
            }});
            window.collectChild = command => document.querySelector('iframe').contentWindow.postMessage(command, '*');
            </script>"#
        );
        let html = CoTaskMemPWSTR::from(html.as_str());
        unsafe { view.webview.NavigateToString(*html.as_ref().as_pcwstr()) }?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if evaluate(
                view,
                "({ marker: window.marker, loaded: document.readyState === 'complete' })",
            )? == serde_json::json!({ "marker": marker, "loaded": true })
            {
                return Ok(());
            }
            anyhow::ensure!(Instant::now() < deadline, "Fixture did not load");
        }
    }

    fn child_state(view: &NativeWebView, focus: bool) -> Result<serde_json::Value> {
        evaluate(
            view,
            if focus {
                "window.childState = null; document.querySelector('iframe').focus(); window.collectChild('focus'); true"
            } else {
                "window.childState = null; window.collectChild('read'); true"
            },
        )?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state = evaluate(view, "window.childState || null")?;
            if !state.is_null() {
                return Ok(state);
            }
            anyhow::ensure!(Instant::now() < deadline, "Iframe did not respond");
        }
    }

    fn nested_state(view: &NativeWebView, create: bool) -> Result<serde_json::Value> {
        let nested_html = r#"<script>let departures = 0;
            addEventListener('blur', () => departures++);
            document.addEventListener('visibilitychange', () => departures++);
            addEventListener('message', () => parent.postMessage({ grandchild: true,
            hidden: document.hidden, visibility: document.visibilityState,
            focused: document.hasFocus(), departures }, '*'));</script>"#;
        let nested_json = serde_json::to_string(nested_html)?.replace('<', "\\u003c");
        let child_html = format!(
            r#"<iframe id=grandchild sandbox="allow-scripts"></iframe>
            <script>addEventListener('message', event => {{
                if (event.data.grandchild) parent.postMessage(event.data, '*');
                else if (event.data === 'read') grandchild.contentWindow.postMessage('read', '*');
            }}); grandchild.onload = () => {{ grandchild.focus();
                grandchild.contentWindow.postMessage('read', '*'); }};
            grandchild.srcdoc = {nested_json};</script>"#
        );
        let child_json = serde_json::to_string(&child_html)?.replace('<', "\\u003c");
        if create {
            evaluate(
                view,
                &format!(
                    r#"window.nestedState = null;
            addEventListener('message', event => {{
                if (event.data.grandchild) window.nestedState = event.data;
            }});
            (() => {{ const frame = document.createElement('iframe');
                frame.sandbox = 'allow-scripts'; frame.srcdoc = {child_json};
                window.nestedOwner = frame; document.body.append(frame); }})(); true"#
                ),
            )?;
        } else {
            evaluate(
                view,
                "window.nestedState = null; nestedOwner.contentWindow.postMessage('read', '*'); true",
            )?;
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state = evaluate(view, "window.nestedState || null")?;
            if !state.is_null() {
                return Ok(state);
            }
            anyhow::ensure!(Instant::now() < deadline, "Nested iframe did not respond");
        }
    }

    #[test]
    fn page_activity_policy_preserves_visibility_forms_and_navigation() -> Result<()> {
        // The parent stays hidden: this test never takes focus from a user's application.
        let parent = ChildHost {
            window: unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Momor activity test"),
                    WS_OVERLAPPED,
                    0,
                    0,
                    640,
                    480,
                    None,
                    None,
                    None,
                    None,
                )
            }?,
            bounds: Cell::new((0, 0, 640, 480)),
            occlusions: RefCell::new(Vec::new()),
        };
        let profile = tempfile::tempdir()?;
        let first = create_view(parent.window, profile.path(), true)?;
        let second = create_view(parent.window, profile.path(), true)?;
        load_fixture(&first, "first")?;
        load_fixture(&second, "second")?;
        let expected =
            serde_json::json!({ "hidden": false, "visibility": "visible", "focused": true });
        assert_eq!(evaluate(&first, "window.initial")?, expected);
        assert_eq!(evaluate(&second, "window.initial")?, expected);
        for view in [&first, &second] {
            view.set_visible(true)?;
            view.set_visible(false)?;
            assert_eq!(
                evaluate(
                    view,
                    "({ hidden: document.hidden, visibility: document.visibilityState, focused: document.hasFocus() })"
                )?,
                expected
            );
            assert_eq!(evaluate(view, "window.departures")?, serde_json::json!(0));
        }
        assert!(!unsafe { IsChild(parent.window, GetFocus()) }.as_bool());
        assert_eq!(
            evaluate(
                &first,
                "second.focus(); first.value = 'kept'; ({ focused: document.activeElement.id, blurs: window.fieldBlurs, value: first.value })"
            )?,
            serde_json::json!({ "focused": "second", "blurs": 1, "value": "kept" })
        );
        let child_expected = serde_json::json!({ "child": true, "hidden": false, "visibility": "visible", "focused": true, "departures": 0 });
        assert!(!first.frame_activity.sessions.borrow().is_empty());
        assert_eq!(child_state(&first, true)?, child_expected);
        first.set_visible(true)?;
        second.set_visible(true)?;
        first.set_visible(false)?;
        assert_eq!(child_state(&first, false)?, child_expected);
        let nested_before = nested_state(&first, true)?;
        assert_eq!(nested_before["hidden"], serde_json::json!(false));
        assert_eq!(nested_before["visibility"], serde_json::json!("visible"));
        assert_eq!(nested_before["departures"], serde_json::json!(0));
        assert!(first.frame_activity.sessions.borrow().len() >= 2);
        first.set_visible(true)?;
        first.set_visible(false)?;
        // A sibling frame can own DOM focus; hiding a tab must not change that internal state.
        assert_eq!(nested_state(&first, false)?, nested_before);
        set_policy(&first, false)?;
        assert_eq!(
            evaluate(&first, "document.hidden")?,
            serde_json::json!(true)
        );
        assert_eq!(
            evaluate(&first, "document.hasFocus()")?,
            serde_json::json!(false)
        );
        assert_eq!(evaluate(&first, "first.value")?, serde_json::json!("kept"));
        assert_eq!(
            nested_state(&first, true)?,
            serde_json::json!({ "grandchild": true, "hidden": true, "visibility": "hidden", "focused": false, "departures": 0 })
        );
        set_policy(&first, true)?;
        assert_eq!(
            evaluate(&first, "document.hidden")?,
            serde_json::json!(false)
        );
        load_fixture(&first, "navigation")?;
        assert_eq!(evaluate(&first, "window.initial")?, expected);
        drop(second);
        drop(first);
        drop(parent);
        Ok(())
    }
}
