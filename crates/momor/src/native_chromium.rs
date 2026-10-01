#[cfg(target_os = "windows")]
mod windows_impl {
    use anyhow::{Context as _, Result};
    use std::{
        cell::RefCell,
        collections::HashMap,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        rc::Rc,
    };
    use windows::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GWL_STYLE, GetWindowLongW, GetWindowThreadProcessId, HWND_TOP,
            IsWindowVisible, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetParent,
            SetWindowLongW, SetWindowPos, ShowWindow, WS_CAPTION, WS_CHILD, WS_MAXIMIZEBOX,
            WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME, WS_VISIBLE,
        },
    };
    use windows::core::BOOL;

    #[derive(Clone, Debug)]
    pub enum NativeEvent {
        Error { message: String },
    }

    struct NativeChromiumState {
        parent: HWND,
        executable: PathBuf,
        profile_dir: PathBuf,
        debug_port: u16,
        process: Option<Child>,
        requested_tabs: HashMap<usize, String>,
        windows: HashMap<usize, HWND>,
        active_tab: usize,
        bounds: (i32, i32, i32, i32),
        has_bounds: bool,
        events: Vec<NativeEvent>,
    }

    #[derive(Clone)]
    pub struct NativeBrowser {
        state: Rc<RefCell<NativeChromiumState>>,
    }

    impl NativeBrowser {
        pub fn new(parent: HWND, profile_dir: &Path) -> Result<Self> {
            if parent.0.is_null() {
                anyhow::bail!("Chromium recebeu uma janela pai inválida");
            }
            let executable = chromium_executable()?;
            let profile_dir = profile_dir.join("chromium-profile");
            std::fs::create_dir_all(&profile_dir).with_context(|| {
                format!(
                    "não foi possível criar o perfil Chromium em {}",
                    profile_dir.display()
                )
            })?;
            let debug_port = std::env::var("MOMOR_CHROMIUM_DEBUG_PORT")
                .ok()
                .and_then(|port| port.parse().ok())
                .unwrap_or_else(|| 9_000 + (std::process::id() % 900) as u16);

            Ok(Self {
                state: Rc::new(RefCell::new(NativeChromiumState {
                    parent,
                    executable,
                    profile_dir,
                    debug_port,
                    process: None,
                    requested_tabs: HashMap::new(),
                    windows: HashMap::new(),
                    active_tab: 0,
                    bounds: (0, 0, 1, 1),
                    has_bounds: false,
                    events: Vec::new(),
                })),
            })
        }

        pub fn debug_port(&self) -> u16 {
            self.state.borrow().debug_port
        }

        pub fn ensure_tab(&self, index: usize, url: &str) -> Result<()> {
            let mut state = self.state.borrow_mut();
            state.requested_tabs.insert(index, url.to_string());
            ensure_process(&mut state)?;
            refresh_embedded_windows(&mut state);
            Ok(())
        }

        pub fn set_bounds(
            &self,
            origin_x: i32,
            origin_y: i32,
            width: i32,
            height: i32,
        ) -> Result<()> {
            let mut state = self.state.borrow_mut();
            state.bounds = (origin_x, origin_y, width.max(1), height.max(1));
            state.has_bounds = true;
            refresh_embedded_windows(&mut state);
            apply_bounds(&state);
            Ok(())
        }

        pub fn set_active_tab(&self, index: usize) -> Result<()> {
            let mut state = self.state.borrow_mut();
            state.active_tab = index;
            refresh_embedded_windows(&mut state);
            for (tab_index, hwnd) in &state.windows {
                unsafe {
                    if !ShowWindow(
                        *hwnd,
                        if *tab_index == index {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    )
                    .as_bool()
                    {
                        tracing::debug!("Chromium não aceitou a atualização de visibilidade");
                    }
                }
            }
            apply_bounds(&state);
            Ok(())
        }

        pub fn open_devtools(&self, index: usize) -> Result<()> {
            post_shortcut(&self.state, index, Shortcut::DevTools)
        }

        pub fn close_tab(&self, index: usize) -> Result<()> {
            let mut state = self.state.borrow_mut();
            state.requested_tabs.remove(&index);
            if let Some(hwnd) = state.windows.remove(&index) {
                unsafe {
                    if !ShowWindow(hwnd, SW_HIDE).as_bool() {
                        tracing::debug!("Chromium não aceitou o fechamento visual da aba");
                    }
                }
            }
            Ok(())
        }

        pub fn drain_events(&self) -> Vec<NativeEvent> {
            let mut state = self.state.borrow_mut();
            std::mem::take(&mut state.events)
        }
    }

    impl Drop for NativeChromiumState {
        fn drop(&mut self) {
            if let Some(mut process) = self.process.take()
                && let Err(error) = process.kill()
            {
                tracing::debug!("falha ao encerrar Chromium incorporado: {error:#}");
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Shortcut {
        DevTools,
    }

    fn post_shortcut(
        state: &Rc<RefCell<NativeChromiumState>>,
        index: usize,
        shortcut: Shortcut,
    ) -> Result<()> {
        let state = state.borrow();
        let hwnd = state
            .windows
            .get(&index)
            .copied()
            .or_else(|| state.windows.get(&state.active_tab).copied())
            .context("janela Chromium ainda não está pronta")?;
        let modifiers = match shortcut {
            Shortcut::DevTools => &[0x11u32, 0x10u32][..],
        };
        let key = 0x49u32;
        unsafe {
            for modifier in modifiers {
                windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(hwnd),
                    windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN,
                    windows::Win32::Foundation::WPARAM(*modifier as usize),
                    windows::Win32::Foundation::LPARAM(0),
                )?;
            }
            windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(hwnd),
                windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN,
                windows::Win32::Foundation::WPARAM(key as usize),
                windows::Win32::Foundation::LPARAM(0),
            )?;
            windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(hwnd),
                windows::Win32::UI::WindowsAndMessaging::WM_KEYUP,
                windows::Win32::Foundation::WPARAM(key as usize),
                windows::Win32::Foundation::LPARAM(0),
            )?;
            for modifier in modifiers.iter().rev() {
                windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(hwnd),
                    windows::Win32::UI::WindowsAndMessaging::WM_KEYUP,
                    windows::Win32::Foundation::WPARAM(*modifier as usize),
                    windows::Win32::Foundation::LPARAM(0),
                )?;
            }
        }
        Ok(())
    }

    fn chromium_executable() -> Result<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(path) = std::env::var_os("MOMOR_CHROMIUM_PATH") {
            candidates.push(PathBuf::from(path));
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let local_app_data = PathBuf::from(local_app_data);
            candidates.push(local_app_data.join("BrowserOS/Application/chrome.exe"));
            candidates.push(local_app_data.join("BrowserClaw/Application/chrome.exe"));
        }
        if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
            return Ok(path);
        }
        anyhow::bail!(
            "Chromium não encontrado; configure MOMOR_CHROMIUM_PATH com o chrome.exe do BrowserOS"
        )
    }

    fn ensure_process(state: &mut NativeChromiumState) -> Result<()> {
        if let Some(process) = state.process.as_mut() {
            match process.try_wait() {
                Ok(None) => return Ok(()),
                Ok(Some(_)) => state.process = None,
                Err(error) => {
                    state.process = None;
                    tracing::debug!("não foi possível consultar o Chromium: {error:#}");
                }
            }
        }
        let mut process = Command::new(&state.executable);
        process
            .arg(format!("--user-data-dir={}", state.profile_dir.display()))
            .arg(format!("--remote-debugging-port={}", state.debug_port))
            .arg("--remote-allow-origins=*")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-session-crashed-bubble")
            .arg("--disable-features=Translate")
            .arg("--app=about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        state.process = Some(process.spawn().with_context(|| {
            format!(
                "não foi possível iniciar Chromium em {}",
                state.executable.display()
            )
        })?);
        Ok(())
    }

    fn refresh_embedded_windows(state: &mut NativeChromiumState) {
        let Some(process) = state.process.as_ref() else {
            return;
        };
        let windows = process_windows(process.id());
        for (position, hwnd) in windows.into_iter().enumerate() {
            let tab_index = state
                .requested_tabs
                .keys()
                .copied()
                .filter(|index| !state.windows.contains_key(index))
                .min()
                .unwrap_or(position);
            state.windows.entry(tab_index).or_insert(hwnd);
            if let Err(error) = embed_window(state.parent, hwnd) {
                state.events.push(NativeEvent::Error {
                    message: format!("falha ao incorporar Chromium: {error:#}"),
                });
            }
        }
    }

    fn process_windows(process_id: u32) -> Vec<HWND> {
        let mut windows = WindowList(Vec::new(), process_id);
        let parameter = LPARAM((&mut windows as *mut WindowList) as isize);
        unsafe {
            let _ = EnumWindows(Some(enumerate_window), parameter);
        }
        windows.0
    }

    unsafe extern "system" fn enumerate_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
        let list = unsafe { &mut *(parameter.0 as *mut WindowList) };
        let mut process_id = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) };
        if process_id == list.1 && unsafe { IsWindowVisible(hwnd).as_bool() } {
            list.0.push(hwnd);
        }
        BOOL(1)
    }

    struct WindowList(Vec<HWND>, u32);

    fn embed_window(parent: HWND, hwnd: HWND) -> Result<()> {
        unsafe {
            SetParent(hwnd, Some(parent)).context("SetParent falhou")?;
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let frame_bits =
                WS_CAPTION.0 | WS_THICKFRAME.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0 | WS_SYSMENU.0;
            let child_style = (style & !frame_bits) | WS_CHILD.0 | WS_VISIBLE.0;
            SetWindowLongW(hwnd, GWL_STYLE, child_style as i32);
            SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                0,
                0,
                1,
                1,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .context("SetWindowPos falhou")?;
        }
        Ok(())
    }

    fn apply_bounds(state: &NativeChromiumState) {
        if !state.has_bounds {
            return;
        }
        let (x, y, width, height) = state.bounds;
        for (tab_index, hwnd) in &state.windows {
            unsafe {
                let _ = SetWindowPos(
                    *hwnd,
                    Some(HWND_TOP),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE
                        | if *tab_index == state.active_tab {
                            SWP_SHOWWINDOW
                        } else {
                            SWP_NOACTIVATE
                        },
                );
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows_impl::{NativeBrowser, NativeEvent};
