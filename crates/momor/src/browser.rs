//! Momor's first-party browser surface.
//!
//! Momor owns the browser surface while Chromium owns web compatibility.
//!
//! The Windows path embeds the installed BrowserOS Chromium window and controls
//! its page targets through the Chrome DevTools Protocol. Obscura remains an
//! explicit fallback for development environments without a Chromium runtime.

#[cfg(target_os = "windows")]
use crate::native_chromium::{NativeBrowser, NativeEvent};
use anyhow::{Context as _, Result, anyhow};
use async_tungstenite::tungstenite::Message;
use base64::Engine as _;
use futures::StreamExt;
use gpui::{
    App, Bounds, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Image, ImageFormat, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point,
    Render, SharedString, Subscription, Task, WeakEntity, Window, anchored, deferred, img,
    prelude::*,
};
#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    time::sleep,
};
use ui::{ContextMenu, ContextMenuEntry, IconName, Label, Tooltip, prelude::*};
use ui_input::InputField;
use url::Url;
use workspace::Workspace;

const DEFAULT_CDP_ENDPOINT: &str = "ws://127.0.0.1:9222/devtools/browser";
const BROWSER_SETTLE_MAX_MS: u64 = 5_000;
const DEFAULT_SEARCH_ENGINE: &str = "https://www.google.com/search";

#[derive(Clone, Debug)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

struct CdpCommand {
    id: u64,
    method: String,
    params: Value,
    session_id: Option<String>,
    reply: oneshot::Sender<Result<Value, String>>,
}

#[derive(Clone)]
pub struct CdpClient {
    commands: mpsc::UnboundedSender<CdpCommand>,
    events: broadcast::Sender<CdpEvent>,
    next_id: Arc<AtomicU64>,
}

impl CdpClient {
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let endpoint_url = Url::parse(endpoint).context("invalid browser CDP endpoint")?;
        if !matches!(endpoint_url.scheme(), "ws" | "wss") {
            anyhow::bail!("browser CDP endpoint must use ws:// or wss://");
        }

        let (socket, _) = async_tungstenite::tokio::connect_async(endpoint)
            .await
            .context("failed to connect to browser CDP")?;
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(64);
        let client = Self {
            commands,
            events: events.clone(),
            next_id: Arc::new(AtomicU64::new(1)),
        };

        tokio::spawn(run_cdp_socket(socket, command_rx, events));
        Ok(client)
    }

    async fn connect_chromium(port: u16) -> Result<Self> {
        let version_url = format!("http://127.0.0.1:{port}/json/version");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(response) = reqwest::get(&version_url).await
                && response.status().is_success()
                && let Ok(body) = response.text().await
                && let Ok(version) = serde_json::from_str::<Value>(&body)
                && let Some(endpoint) = version.get("webSocketDebuggerUrl").and_then(Value::as_str)
            {
                return Self::connect(endpoint).await;
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "Chromium não abriu o endpoint CDP em http://127.0.0.1:{port}/json/version"
                );
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    async fn launch_and_connect() -> Result<Self> {
        let ready_path = std::env::temp_dir().join(format!(
            "momor-obscura-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let storage_dir = embedded_storage_dir();
        let server_ready_path = ready_path.clone();
        tokio::spawn(async move {
            if let Err(error) = obscura_cdp::start_with_serve_options_limit_and_ready_file(
                0,
                "127.0.0.1",
                None,
                false,
                None,
                false,
                Some(storage_dir),
                false,
                4,
                Some(&server_ready_path),
            )
            .await
            {
                tracing::error!("embedded Obscura server stopped: {error:#}");
            }
        });

        let ready = wait_for_ready_file(&ready_path).await?;
        if let Err(error) = tokio::fs::remove_file(&ready_path).await {
            tracing::debug!("failed to remove Obscura ready file: {error:#}");
        }
        let endpoint = format!("ws://{}:{}{}", ready.host, ready.port, ready.websocket_path);
        Self::connect(&endpoint).await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        self.send_with_session(method, params, None).await
    }

    pub async fn send_with_session(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, response) = oneshot::channel();
        self.commands
            .send(CdpCommand {
                id,
                method: method.to_string(),
                params,
                session_id: session_id.map(str::to_owned),
                reply,
            })
            .map_err(|_| anyhow!("Obscura CDP connection closed"))?;

        response
            .await
            .map_err(|_| anyhow!("Obscura CDP connection closed"))?
            .map_err(|error| anyhow!(error))
    }

    pub async fn create_page(&self, url: &str) -> Result<BrowserPage> {
        let created = self
            .send(
                "Target.createTarget",
                json!({ "url": url, "newWindow": false, "background": false }),
            )
            .await?;
        let target_id = created
            .get("targetId")
            .and_then(Value::as_str)
            .context("browser did not return a target id")?
            .to_string();
        let attached = self
            .send(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
            )
            .await?;
        let session_id = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .context("browser did not return a target session id")?
            .to_string();

        self.send_with_session("Page.enable", json!({}), Some(&session_id))
            .await?;
        self.send_with_session("Runtime.enable", json!({}), Some(&session_id))
            .await?;
        self.send_with_session("Network.enable", json!({}), Some(&session_id))
            .await?;
        self.send_with_session("DOM.enable", json!({}), Some(&session_id))
            .await?;

        Ok(BrowserPage {
            target_id,
            session_id,
            url: url.to_string(),
        })
    }

    async fn connect_existing_page(&self, url: &str) -> Result<BrowserPage> {
        let targets = self.send("Target.getTargets", json!({})).await?;
        let target = targets
            .get("targetInfos")
            .and_then(Value::as_array)
            .and_then(|targets| {
                targets
                    .iter()
                    .find(|target| {
                        target.get("type").and_then(Value::as_str) == Some("page")
                            && target
                                .get("url")
                                .and_then(Value::as_str)
                                .is_some_and(|url| {
                                    url == "about:blank" || url.starts_with("chrome://newtab")
                                })
                    })
                    .or_else(|| {
                        targets.iter().find(|target| {
                            target.get("type").and_then(Value::as_str) == Some("page")
                        })
                    })
            })
            .context("Chromium não retornou uma página incorporada")?;
        let target_id = target
            .get("targetId")
            .and_then(Value::as_str)
            .context("Chromium não retornou o id da página incorporada")?;
        let attached = self
            .send(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
            )
            .await?;
        let session_id = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .context("Chromium não retornou a sessão da página incorporada")?
            .to_string();
        self.send_with_session("Page.enable", json!({}), Some(&session_id))
            .await?;
        self.send_with_session("Runtime.enable", json!({}), Some(&session_id))
            .await?;
        self.send_with_session("Network.enable", json!({}), Some(&session_id))
            .await?;
        self.send_with_session("DOM.enable", json!({}), Some(&session_id))
            .await?;
        let mut page = BrowserPage {
            target_id: target_id.to_string(),
            session_id,
            url: target
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("about:blank")
                .to_string(),
        };
        self.navigate(&mut page, url).await?;
        Ok(page)
    }

    pub async fn close_page(&self, page: &BrowserPage) -> Result<()> {
        self.send("Target.closeTarget", json!({ "targetId": page.target_id }))
            .await
            .map(|_| ())
    }

    pub async fn set_viewport(&self, page: &BrowserPage, width: u32, height: u32) -> Result<()> {
        self.send_with_session(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": 1,
                "mobile": false,
            }),
            Some(&page.session_id),
        )
        .await
        .map(|_| ())
    }

    pub async fn set_download_behavior(&self, path: &Path) -> Result<()> {
        self.send(
            "Browser.setDownloadBehavior",
            json!({
                "behavior": "allow",
                "downloadPath": path.to_string_lossy(),
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn navigate(&self, page: &mut BrowserPage, url: &str) -> Result<()> {
        self.send_with_session(
            "Page.navigate",
            json!({ "url": url }),
            Some(&page.session_id),
        )
        .await?;
        page.url = url.to_string();
        Ok(())
    }

    async fn settle_page(&self, page: &BrowserPage) -> Result<()> {
        self.send_with_session(
            "Page.settle",
            json!({ "maxMs": BROWSER_SETTLE_MAX_MS }),
            Some(&page.session_id),
        )
        .await
        .map(|_| ())
    }

    pub async fn navigate_history(&self, page: &mut BrowserPage, delta: i32) -> Result<()> {
        let history = self
            .send_with_session(
                "Page.getNavigationHistory",
                json!({}),
                Some(&page.session_id),
            )
            .await?;
        let current_index = history
            .get("currentIndex")
            .and_then(Value::as_i64)
            .context("Obscura did not return the current history index")?;
        let target_index = current_index + i64::from(delta);
        let entries = history
            .get("entries")
            .and_then(Value::as_array)
            .context("Obscura did not return navigation history")?;
        let target = entries
            .get(usize::try_from(target_index).unwrap_or(usize::MAX))
            .context("no browser history entry in that direction")?;
        let entry_id = target
            .get("id")
            .and_then(Value::as_i64)
            .context("Obscura did not return a history entry id")?;
        self.send_with_session(
            "Page.navigateToHistoryEntry",
            json!({ "entryId": entry_id }),
            Some(&page.session_id),
        )
        .await?;
        page.url = target
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(&page.url)
            .to_string();
        Ok(())
    }

    pub async fn navigation_history(
        &self,
        page: &BrowserPage,
    ) -> Result<(usize, Vec<BrowserHistoryEntry>)> {
        let history = self
            .send_with_session(
                "Page.getNavigationHistory",
                json!({}),
                Some(&page.session_id),
            )
            .await?;
        let current_index = history
            .get("currentIndex")
            .and_then(Value::as_u64)
            .context("Obscura did not return the current history index")?
            as usize;
        let entries = history
            .get("entries")
            .and_then(Value::as_array)
            .context("Obscura did not return navigation history")?
            .iter()
            .filter_map(|entry| {
                Some(BrowserHistoryEntry {
                    id: entry.get("id")?.as_i64()?,
                    url: entry.get("url")?.as_str()?.to_string(),
                    title: entry
                        .get("title")
                        .and_then(Value::as_str)
                        .filter(|title| !title.is_empty())
                        .or_else(|| entry.get("userTypedURL").and_then(Value::as_str))
                        .unwrap_or("")
                        .to_string(),
                })
            })
            .collect();
        Ok((current_index, entries))
    }

    pub async fn capture_screenshot(&self, page: &BrowserPage) -> Result<Vec<u8>> {
        let response = self
            .send_with_session(
                "Page.captureScreenshot",
                json!({ "format": "png" }),
                Some(&page.session_id),
            )
            .await?;
        let encoded = response
            .get("data")
            .and_then(Value::as_str)
            .context("Obscura did not return screenshot data")?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("Obscura returned invalid screenshot data")
    }

    pub async fn start_screencast(
        &self,
        page: &BrowserPage,
    ) -> Result<broadcast::Receiver<CdpEvent>> {
        let events = self.subscribe();
        self.send_with_session(
            "Page.startScreencast",
            json!({
                "format": "png",
                "maxWidth": 2048,
                "maxHeight": 2048,
                "everyNthFrame": 1,
            }),
            Some(&page.session_id),
        )
        .await?;
        Ok(events)
    }

    async fn acknowledge_screencast_frame(
        &self,
        page: &BrowserPage,
        session_id: i64,
    ) -> Result<()> {
        self.send_with_session(
            "Page.screencastFrameAck",
            json!({ "sessionId": session_id }),
            Some(&page.session_id),
        )
        .await
        .map(|_| ())
    }

    pub async fn dispatch_mouse_event(
        &self,
        page: &BrowserPage,
        event_type: &str,
        x: f32,
        y: f32,
        click_count: usize,
        button: &str,
        modifiers: u8,
    ) -> Result<()> {
        self.send_with_session(
            "Input.dispatchMouseEvent",
            json!({
                "type": event_type,
                "x": x,
                "y": y,
                "button": button,
                "buttons": if event_type == "mousePressed" { 1 } else { 0 },
                "clickCount": click_count,
                "modifiers": modifiers,
            }),
            Some(&page.session_id),
        )
        .await
        .map(|_| ())
    }

    pub async fn dispatch_mouse_wheel(
        &self,
        page: &BrowserPage,
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
    ) -> Result<()> {
        self.send_with_session(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseWheel",
                "x": x,
                "y": y,
                "deltaX": delta_x,
                "deltaY": delta_y,
            }),
            Some(&page.session_id),
        )
        .await
        .map(|_| ())
    }

    pub async fn dispatch_key_event(
        &self,
        page: &BrowserPage,
        event_type: &str,
        key: &str,
        text: &str,
        modifiers: u8,
    ) -> Result<()> {
        self.send_with_session(
            "Input.dispatchKeyEvent",
            json!({
                "type": event_type,
                "key": cdp_key_value(key),
                "code": cdp_key_code(key),
                "text": text,
                "modifiers": modifiers,
                "windowsVirtualKeyCode": cdp_virtual_key_code(key),
                "nativeVirtualKeyCode": cdp_virtual_key_code(key),
            }),
            Some(&page.session_id),
        )
        .await
        .map(|_| ())
    }

    pub async fn evaluate(&self, page: &BrowserPage, expression: &str) -> Result<Value> {
        let response = self
            .send_with_session(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
                Some(&page.session_id),
            )
            .await?;
        Ok(response
            .get("result")
            .and_then(|result| result.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub async fn print_to_pdf(&self, page: &BrowserPage) -> Result<Vec<u8>> {
        let response = self
            .send_with_session(
                "Page.printToPDF",
                json!({ "printBackground": true, "preferCSSPageSize": true }),
                Some(&page.session_id),
            )
            .await?;
        let data = response
            .get("data")
            .and_then(Value::as_str)
            .context("Obscura did not return PDF data")?;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("Obscura returned invalid PDF data")
    }
}

async fn run_cdp_socket<S>(
    mut socket: async_tungstenite::WebSocketStream<S>,
    mut commands: mpsc::UnboundedReceiver<CdpCommand>,
    events: broadcast::Sender<CdpEvent>,
) where
    S: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + 'static,
{
    let mut pending = HashMap::<u64, oneshot::Sender<Result<Value, String>>>::new();
    let close_error = |pending: &mut HashMap<u64, oneshot::Sender<Result<Value, String>>>| {
        for (_, reply) in pending.drain() {
            let _ = reply.send(Err("Obscura CDP connection closed".to_string()));
        }
    };

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    close_error(&mut pending);
                    return;
                };
                let mut message = json!({
                    "id": command.id,
                    "method": command.method,
                    "params": command.params,
                });
                if let Some(session_id) = command.session_id {
                    message["sessionId"] = Value::String(session_id);
                }
                if socket.send(Message::Text(message.to_string().into())).await.is_err() {
                    let _ = command.reply.send(Err("failed to write to Obscura CDP".to_string()));
                    close_error(&mut pending);
                    return;
                }
                pending.insert(command.id, command.reply);
            }
            message = socket.next() => {
                let Some(message) = message else {
                    close_error(&mut pending);
                    return;
                };
                let Ok(message) = message else {
                    close_error(&mut pending);
                    return;
                };
                let text = match message {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_) => {
                        close_error(&mut pending);
                        return;
                    }
                    _ => continue,
                };
                let Ok(message) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = message.get("id").and_then(Value::as_u64) {
                    if let Some(reply) = pending.remove(&id) {
                        if let Some(error) = message.get("error") {
                            let _ = reply.send(Err(error.to_string()));
                        } else {
                            let _ = reply.send(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
                        }
                    }
                } else if let Some(method) = message.get("method").and_then(Value::as_str) {
                    let _ = events.send(CdpEvent {
                        method: method.to_string(),
                        params: message.get("params").cloned().unwrap_or(Value::Null),
                        session_id: message.get("sessionId").and_then(Value::as_str).map(str::to_owned),
                    });
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct BrowserPage {
    pub target_id: String,
    pub session_id: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct BrowserHistoryEntry {
    pub id: i64,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Deserialize)]
struct ObscuraReadyFile {
    host: String,
    port: u16,
    websocket_path: String,
}

async fn wait_for_ready_file(ready_path: &std::path::Path) -> Result<ObscuraReadyFile> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(bytes) = tokio::fs::read(ready_path).await {
            return serde_json::from_slice(&bytes).context("invalid Obscura ready file");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("embedded Obscura did not become ready within 20 seconds");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn embedded_storage_dir() -> PathBuf {
    std::env::var_os("MOMOR_OBSCURA_STORAGE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("momor-obscura-profile"))
}

fn browser_download_dir() -> Result<PathBuf> {
    let path = std::env::var_os("MOMOR_OBSCURA_DOWNLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| embedded_storage_dir().join("downloads"));
    std::fs::create_dir_all(&path).with_context(|| {
        format!(
            "failed to create browser download directory {}",
            path.display()
        )
    })?;
    Ok(path)
}

fn read_saved_urls(key: &str, cx: &App) -> Vec<String> {
    let value = match db::kvp::KeyValueStore::global(cx).read_kvp(key) {
        Ok(Some(value)) => value,
        Ok(None) => return Vec::new(),
        Err(error) => {
            tracing::debug!("reading browser saved URLs failed: {error:#}");
            return Vec::new();
        }
    };
    match serde_json::from_str(&value) {
        Ok(urls) => urls,
        Err(error) => {
            tracing::debug!("parsing browser saved URLs failed: {error:#}");
            Vec::new()
        }
    }
}

fn default_endpoint() -> String {
    std::env::var("MOMOR_OBSCURA_CDP_URL").unwrap_or_else(|_| DEFAULT_CDP_ENDPOINT.to_string())
}

fn normalize_url(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok("about:blank".to_string());
    }
    if !input.contains("://")
        && !input.starts_with("about:")
        && !input.contains('.')
        && !input.starts_with("localhost")
        && !input.starts_with('[')
        && !input.contains(':')
    {
        let mut search_url = Url::parse(DEFAULT_SEARCH_ENGINE)?;
        search_url.query_pairs_mut().append_pair("q", input);
        return Ok(search_url.to_string());
    }
    let candidate = if input.contains("://") || input.starts_with("about:") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let url = Url::parse(&candidate).context("invalid browser URL")?;
    if !matches!(url.scheme(), "http" | "https" | "about") {
        anyhow::bail!(
            "Obscura browser does not allow the {}:// scheme",
            url.scheme()
        );
    }
    Ok(url.to_string())
}

fn cdp_key_code(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "enter" => "Enter".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" => "Delete".to_string(),
        "tab" => "Tab".to_string(),
        "escape" | "esc" => "Escape".to_string(),
        "arrowleft" => "ArrowLeft".to_string(),
        "arrowright" => "ArrowRight".to_string(),
        "arrowup" => "ArrowUp".to_string(),
        "arrowdown" => "ArrowDown".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        key if key.len() == 1 && key.chars().all(|character| character.is_ascii_alphabetic()) => {
            format!("Key{}", key.to_ascii_uppercase())
        }
        key if key.len() == 1 && key.chars().all(|character| character.is_ascii_digit()) => {
            format!("Digit{key}")
        }
        key => key.to_string(),
    }
}

fn cdp_key_value(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "enter" => "Enter".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" => "Delete".to_string(),
        "tab" => "Tab".to_string(),
        "escape" | "esc" => "Escape".to_string(),
        "space" => " ".to_string(),
        "arrowleft" => "ArrowLeft".to_string(),
        "arrowright" => "ArrowRight".to_string(),
        "arrowup" => "ArrowUp".to_string(),
        "arrowdown" => "ArrowDown".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        "shift" => "Shift".to_string(),
        "control" | "ctrl" => "Control".to_string(),
        "alt" => "Alt".to_string(),
        "meta" | "cmd" | "command" => "Meta".to_string(),
        key => key.to_string(),
    }
}

fn cdp_virtual_key_code(key: &str) -> u32 {
    match key.to_ascii_lowercase().as_str() {
        "backspace" => 8,
        "tab" => 9,
        "enter" => 13,
        "shift" => 16,
        "control" | "ctrl" => 17,
        "alt" => 18,
        "escape" | "esc" => 27,
        "space" => 32,
        "pageup" => 33,
        "pagedown" => 34,
        "end" => 35,
        "home" => 36,
        "arrowleft" => 37,
        "arrowup" => 38,
        "arrowright" => 39,
        "arrowdown" => 40,
        "delete" => 46,
        key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() => {
            key.as_bytes()[0].to_ascii_uppercase() as u32
        }
        key if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() => key.as_bytes()[0] as u32,
        _ => 0,
    }
}

fn cdp_modifiers(modifiers: &gpui::Modifiers) -> u8 {
    u8::from(modifiers.alt)
        | (u8::from(modifiers.control) << 1)
        | (u8::from(modifiers.platform) << 2)
        | (u8::from(modifiers.shift) << 3)
}

struct BrowserNavigation {
    client: CdpClient,
    page: BrowserPage,
    screenshot: Vec<u8>,
    screencast: Option<broadcast::Receiver<CdpEvent>>,
    events: broadcast::Receiver<CdpEvent>,
}

#[derive(Clone)]
struct BrowserTab {
    page: Option<BrowserPage>,
    current_url: String,
    title: String,
    image: Option<Arc<Image>>,
    status: String,
    history: Vec<BrowserHistoryEntry>,
    history_index: usize,
    loading: bool,
}

impl BrowserTab {
    fn new() -> Self {
        Self {
            page: None,
            current_url: "about:blank".to_string(),
            title: "Nova aba".to_string(),
            image: None,
            status: "Digite uma URL para navegar".to_string(),
            history: Vec::new(),
            history_index: 0,
            loading: false,
        }
    }

    fn label(&self) -> SharedString {
        if !self.title.is_empty() && self.title != "Nova aba" && self.title != "about:blank" {
            return self.title.clone().into();
        }
        self.page
            .as_ref()
            .map(|page| page.url.clone())
            .filter(|url| !url.is_empty() && url != "about:blank")
            .or_else(|| {
                (!self.current_url.is_empty() && self.current_url != "about:blank")
                    .then(|| self.current_url.clone())
            })
            .unwrap_or_else(|| self.title.clone())
            .into()
    }
}

pub struct BrowserPanel {
    focus_handle: FocusHandle,
    _workspace: WeakEntity<Workspace>,
    address_bar: Option<Entity<InputField>>,
    _address_subscription: Option<Subscription>,
    client: Option<CdpClient>,
    tabs: Vec<BrowserTab>,
    active_tab: usize,
    navigation_task: Option<Task<()>>,
    screencast_tasks: HashMap<String, Task<()>>,
    download_events_task: Option<Task<()>>,
    download_status: Option<String>,
    content_bounds: Option<Bounds<Pixels>>,
    viewport_size: Option<(u32, u32)>,
    viewport_auto: bool,
    viewport_scale: f32,
    last_mouse_move: Option<Instant>,
    #[cfg(target_os = "windows")]
    native_browser: Option<NativeBrowser>,
    #[cfg(target_os = "windows")]
    native_events_task: Option<Task<()>>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    devtools_open: bool,
    devtools_tab: DevtoolsTab,
    devtools_output: Vec<String>,
    history_open: bool,
    favorites: Vec<String>,
    quick_access: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DevtoolsTab {
    Console,
    Network,
    Dom,
}

impl DevtoolsTab {
    fn label(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::Network => "Rede",
            Self::Dom => "DOM",
        }
    }
}

impl BrowserPanel {
    pub fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            _workspace: workspace,
            address_bar: None,
            _address_subscription: None,
            client: None,
            tabs: vec![BrowserTab::new()],
            active_tab: 0,
            navigation_task: None,
            screencast_tasks: HashMap::new(),
            download_events_task: None,
            download_status: None,
            content_bounds: None,
            viewport_size: None,
            viewport_auto: true,
            viewport_scale: 1.0,
            last_mouse_move: None,
            #[cfg(target_os = "windows")]
            native_browser: None,
            #[cfg(target_os = "windows")]
            native_events_task: None,
            context_menu: None,
            devtools_open: false,
            devtools_tab: DevtoolsTab::Console,
            devtools_output: Vec::new(),
            history_open: false,
            favorites: read_saved_urls("momor_browser_favorites", cx),
            quick_access: read_saved_urls("momor_browser_quick_access", cx),
        }
    }

    fn active_tab(&self) -> Option<&BrowserTab> {
        self.tabs.get(self.active_tab)
    }

    fn active_tab_mut(&mut self) -> Option<&mut BrowserTab> {
        self.tabs.get_mut(self.active_tab)
    }

    fn active_page(&self) -> Option<BrowserPage> {
        self.active_tab().and_then(|tab| tab.page.clone())
    }

    fn active_url(&self) -> String {
        self.active_tab()
            .map(|tab| {
                tab.page
                    .as_ref()
                    .map(|page| page.url.clone())
                    .unwrap_or_else(|| tab.current_url.clone())
            })
            .unwrap_or_else(|| "about:blank".to_string())
    }

    fn sync_address_bar(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(address_bar) = self.address_bar.clone() else {
            return;
        };
        let text = self.active_url();
        address_bar.update(cx, |field, cx| field.set_text(&text, window, cx));
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(BrowserTab::new());
        self.active_tab = self.tabs.len().saturating_sub(1);
        self.viewport_size = None;
        self.viewport_auto = true;
        self.last_mouse_move = None;
        #[cfg(target_os = "windows")]
        if let Some(native_browser) = &self.native_browser {
            if let Err(error) = native_browser.ensure_tab(self.active_tab, "about:blank") {
                tracing::debug!("failed to create native browser tab: {error:#}");
            }
            if let Err(error) = native_browser.set_active_tab(self.active_tab) {
                tracing::debug!("failed to activate native browser tab: {error:#}");
            }
        }
        self.sync_address_bar(window, cx);
        cx.notify();
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        self.active_tab = index;
        self.viewport_size = None;
        self.viewport_auto = true;
        self.last_mouse_move = None;
        #[cfg(target_os = "windows")]
        if let Some(native_browser) = &self.native_browser {
            if let Err(error) = native_browser.ensure_tab(index, &self.active_url()) {
                tracing::debug!("failed to prepare native browser tab: {error:#}");
            }
            if let Err(error) = native_browser.set_active_tab(index) {
                tracing::debug!("failed to activate native browser tab: {error:#}");
            }
        }
        self.sync_address_bar(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        #[cfg(target_os = "windows")]
        if let Some(native_browser) = &self.native_browser {
            if let Err(error) = native_browser.close_tab(index) {
                tracing::debug!("failed to close native browser tab: {error:#}");
            }
        }
        if let (Some(client), Some(page)) = (self.client.clone(), tab.page) {
            let task = gpui_tokio::Tokio::handle(cx).spawn(async move {
                if let Err(error) = client.close_page(&page).await {
                    tracing::warn!("failed to close Obscura target: {error:#}");
                }
            });
            drop(task);
        }
        if self.tabs.is_empty() {
            self.tabs.push(BrowserTab::new());
        }
        if self.active_tab > index {
            self.active_tab = self.active_tab.saturating_sub(1);
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }
        self.viewport_size = None;
        self.viewport_auto = true;
        self.last_mouse_move = None;
        cx.notify();
    }

    fn ensure_address_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.address_bar.is_some() {
            return;
        }
        let field = cx.new(|cx| {
            InputField::new(window, cx, "Digite uma URL")
                .label_min_width(px(0.))
                .start_icon(IconName::Public)
        });
        let editor = field.read(cx).editor().clone();
        let weak = cx.weak_entity();
        self._address_subscription = Some(editor.subscribe(
            Box::new(move |event, _window, cx| {
                if matches!(event, ui_input::ErasedEditorEvent::BufferEdited) {
                    weak.update(cx, |_this, cx| cx.notify()).ok();
                }
            }),
            window,
            cx,
        ));
        self.address_bar = Some(field);
    }

    #[cfg(target_os = "windows")]
    fn ensure_native_browser(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.native_browser.is_none() {
            let parent = match window.window_handle() {
                Ok(handle) => match handle.as_raw() {
                    RawWindowHandle::Win32(handle) => {
                        windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut std::ffi::c_void)
                    }
                    _ => {
                        if let Some(tab) = self.active_tab_mut() {
                            tab.status = "O navegador nativo requer uma janela Win32".to_string();
                        }
                        return;
                    }
                },
                Err(error) => {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.status = format!("Falha ao obter a janela do navegador: {error:#}");
                    }
                    return;
                }
            };
            match NativeBrowser::new(parent, &embedded_storage_dir()) {
                Ok(native_browser) => self.native_browser = Some(native_browser),
                Err(error) => {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.status = format!("Chromium indisponível: {error:#}");
                    }
                    return;
                }
            }
        }

        let scale = window.scale_factor();
        let origin_x = (bounds.origin.x.as_f32() * scale).round() as i32;
        let origin_y = (bounds.origin.y.as_f32() * scale).round() as i32;
        let width = (bounds.size.width.as_f32() * scale).round().max(1.0) as i32;
        let height = (bounds.size.height.as_f32() * scale).round().max(1.0) as i32;
        if let Some(native_browser) = &self.native_browser {
            if let Err(error) = native_browser.set_bounds(origin_x, origin_y, width, height) {
                tracing::debug!("failed to resize native browser: {error:#}");
            }
            if let Err(error) = native_browser.ensure_tab(self.active_tab, &self.active_url()) {
                tracing::debug!("failed to ensure active native tab: {error:#}");
            }
            if let Err(error) = native_browser.set_active_tab(self.active_tab) {
                tracing::debug!("failed to show active native tab: {error:#}");
            }
        }
        self.start_native_event_pump(window, cx);
    }

    #[cfg(target_os = "windows")]
    fn start_native_event_pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.native_events_task.is_some() {
            return;
        }
        self.native_events_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let update = this.update_in(cx, |this, _window, cx| {
                    let Some(native_browser) = &this.native_browser else {
                        return true;
                    };
                    let events = native_browser.drain_events();
                    this.apply_native_events(events, cx);
                    true
                });
                if update.is_err() {
                    break;
                }
            }
        }));
    }

    #[cfg(target_os = "windows")]
    fn apply_native_events(&mut self, events: Vec<NativeEvent>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                NativeEvent::Error { message } => {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.status = message;
                        tab.loading = false;
                    }
                }
            }
        }
        cx.notify();
    }

    fn browser_coordinates(&self, position: Point<Pixels>) -> Option<(f32, f32)> {
        let bounds = self.content_bounds?;
        let local = position - bounds.origin;
        if local.x < px(0.)
            || local.y < px(0.)
            || local.x > bounds.size.width
            || local.y > bounds.size.height
        {
            return None;
        }
        let display_width = f32::from(bounds.size.width);
        let display_height = f32::from(bounds.size.height);
        if display_width <= 0.0 || display_height <= 0.0 {
            return None;
        }
        let (viewport_width, viewport_height) = self.viewport_size.unwrap_or((
            display_width.round().max(1.0) as u32,
            display_height.round().max(1.0) as u32,
        ));
        Some((
            f32::from(local.x) * viewport_width as f32 / display_width,
            f32::from(local.y) * viewport_height as f32 / display_height,
        ))
    }

    fn dispatch_mouse(
        &mut self,
        event_type: &'static str,
        position: Point<Pixels>,
        click_count: usize,
        button: &'static str,
        modifiers: u8,
        cx: &mut Context<Self>,
    ) {
        #[cfg(target_os = "windows")]
        if self.native_browser.is_some() {
            return;
        }
        if event_type == "mouseMoved" {
            let now = Instant::now();
            if self
                .last_mouse_move
                .is_some_and(|last| now.duration_since(last) < Duration::from_millis(16))
            {
                return;
            }
            self.last_mouse_move = Some(now);
        }
        let (Some(client), Some(page), Some((x, y))) = (
            self.client.clone(),
            self.active_page(),
            self.browser_coordinates(position),
        ) else {
            return;
        };
        gpui_tokio::Tokio::handle(cx).spawn(async move {
            if let Err(error) = client
                .dispatch_mouse_event(&page, event_type, x, y, click_count, button, modifiers)
                .await
            {
                tracing::debug!("browser mouse event failed: {error:#}");
            }
        });
    }

    fn sync_history(&mut self, tab_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(page)) = (
            self.client.clone(),
            self.tabs.get(tab_index).and_then(|tab| tab.page.clone()),
        ) else {
            return;
        };
        let task =
            gpui_tokio::Tokio::spawn_result(
                cx,
                async move { client.navigation_history(&page).await },
            );
        cx.spawn_in(window, async move |this, cx| {
            if let Ok((history_index, history)) = task.await {
                if let Err(error) = this.update(cx, |this, cx| {
                    if let Some(tab) = this.tabs.get_mut(tab_index) {
                        tab.history_index = history_index;
                        tab.history = history;
                        tab.loading = false;
                    }
                    cx.notify();
                }) {
                    tracing::debug!("browser history state update failed: {error:#}");
                }
            }
        })
        .detach();
    }

    fn push_devtools_output(&mut self, line: impl Into<String>, cx: &mut Context<Self>) {
        self.devtools_output.push(line.into());
        if self.devtools_output.len() > 120 {
            let remove_count = self.devtools_output.len() - 120;
            self.devtools_output.drain(0..remove_count);
        }
        cx.notify();
    }

    fn toggle_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history_open = !self.history_open;
        if self.history_open {
            self.sync_history(self.active_tab, window, cx);
        }
        cx.notify();
    }

    fn save_current_url(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let url = self.active_url();
        if url == "about:blank" {
            return;
        }
        let urls = if key == "momor_browser_favorites" {
            &mut self.favorites
        } else {
            &mut self.quick_access
        };
        if !urls.iter().any(|saved| saved == &url) {
            urls.push(url.clone());
        }
        let value = match serde_json::to_string(urls) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!("serializing browser saved URLs failed: {error:#}");
                return;
            }
        };
        let store = db::kvp::KeyValueStore::global(cx).clone();
        db::write_and_log(cx, move || async move {
            store.write_kvp(key.into(), value).await
        });
        if let Some(tab) = self.active_tab_mut() {
            tab.status = if key == "momor_browser_favorites" {
                "Adicionado aos favoritos".to_string()
            } else {
                "Adicionado à Discagem Rápida".to_string()
            };
        }
        cx.notify();
    }

    fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        let url = self.active_url();
        if url == "about:blank" {
            return;
        }

        let removed = if let Some(index) = self.favorites.iter().position(|saved| saved == &url) {
            self.favorites.remove(index);
            true
        } else {
            self.favorites.push(url);
            false
        };
        let value = match serde_json::to_string(&self.favorites) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!("serializing browser favorites failed: {error:#}");
                return;
            }
        };
        let store = db::kvp::KeyValueStore::global(cx).clone();
        db::write_and_log(cx, move || async move {
            store
                .write_kvp("momor_browser_favorites".into(), value)
                .await
        });
        if let Some(tab) = self.active_tab_mut() {
            tab.status = if removed {
                "Removido dos favoritos".to_string()
            } else {
                "Adicionado aos favoritos".to_string()
            };
        }
        cx.notify();
    }

    fn navigate_history_entry(
        &mut self,
        history_entry: BrowserHistoryEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_index = self.active_tab;
        let (Some(client), Some(mut page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        if let Some(tab) = self.active_tab_mut() {
            tab.loading = true;
            tab.status = format!("Navegando para {}...", history_entry.url);
            tab.image = None;
        }
        self.navigation_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                client
                    .send_with_session(
                        "Page.navigateToHistoryEntry",
                        json!({ "entryId": history_entry.id }),
                        Some(&page.session_id),
                    )
                    .await?;
                page.url = history_entry.url;
                client.settle_page(&page).await?;
                let screenshot = client.capture_screenshot(&page).await?;
                anyhow::Ok((page, screenshot))
            }
            .await;
            if let Err(error) = this.update_in(cx, |this, window, cx| {
                if let Some(tab) = this.tabs.get_mut(tab_index) {
                    match result {
                        Ok((page, screenshot)) => {
                            tab.page = Some(page.clone());
                            tab.image =
                                Some(Arc::new(Image::from_bytes(ImageFormat::Png, screenshot)));
                            tab.status = page.url.clone();
                            tab.loading = false;
                            this.sync_address_bar(window, cx);
                            this.sync_history(tab_index, window, cx);
                        }
                        Err(error) => {
                            tab.loading = false;
                            tab.status = format!("Falha no histórico: {error:#}");
                        }
                    }
                }
                cx.notify();
            }) {
                tracing::debug!("browser history entry UI update failed: {error:#}");
            }
        }));
    }

    fn open_context_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus_handle = self.focus_handle.clone();
        let weak = cx.weak_entity();
        let can_back = self.active_tab().is_some_and(|tab| tab.history_index > 0);
        let can_forward = self
            .active_tab()
            .is_some_and(|tab| tab.history_index.saturating_add(1) < tab.history.len());
        let inspection_position = position;
        let menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let back = weak.clone();
            let forward = weak.clone();
            let reload = weak.clone();
            let copy = weak.clone();
            let screenshot = weak.clone();
            let pdf = weak.clone();
            let print = weak.clone();
            let dark = weak.clone();
            let quick_access = weak.clone();
            let favorite = weak.clone();
            let inspect = weak.clone();
            let source = weak.clone();
            let devtools = weak.clone();
            let history = weak.clone();
            let scale_down = weak.clone();
            let scale_reset = weak.clone();
            let scale_up = weak.clone();
            menu.context(focus_handle)
                .item(
                    ContextMenuEntry::new("Voltar")
                        .icon(IconName::ArrowLeft)
                        .disabled(!can_back)
                        .handler(move |_window, cx| {
                            if let Err(error) = back.update_in(cx, |this, window, cx| {
                                this.navigate_history(-1, window, cx);
                            }) {
                                tracing::debug!("browser context back failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Avançar")
                        .icon(IconName::ArrowRight)
                        .disabled(!can_forward)
                        .handler(move |_window, cx| {
                            if let Err(error) = forward.update_in(cx, |this, window, cx| {
                                this.navigate_history(1, window, cx);
                            }) {
                                tracing::debug!("browser context forward failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Recarregar")
                        .icon(IconName::RotateCw)
                        .handler(move |_window, cx| {
                            if let Err(error) = reload.update_in(cx, |this, window, cx| {
                                this.reload(window, cx);
                            }) {
                                tracing::debug!("browser context reload failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Ativar tela cheia")
                        .icon(IconName::Maximize)
                        .handler(move |window, _cx| window.toggle_fullscreen()),
                )
                .item(
                    ContextMenuEntry::new("Histórico")
                        .icon(IconName::HistoryRerun)
                        .handler(move |_window, cx| {
                            if let Err(error) = history.update_in(cx, |this, window, cx| {
                                this.toggle_history(window, cx);
                            }) {
                                tracing::debug!("opening browser history failed: {error:#}");
                            }
                        }),
                )
                .separator()
                .item(
                    ContextMenuEntry::new("Copiar endereço")
                        .icon(IconName::Copy)
                        .handler(move |_, cx| {
                            if let Err(error) = copy.update(cx, |this, cx| {
                                if let Some(url) = this.active_page().map(|page| page.url) {
                                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                                }
                            }) {
                                tracing::debug!("copying browser address failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Adicionar à Discagem Rápida")
                        .icon(IconName::Plus)
                        .handler(move |_, cx| {
                            if let Err(error) = quick_access.update(cx, |this, cx| {
                                this.save_current_url("momor_browser_quick_access", cx);
                            }) {
                                tracing::debug!("saving browser quick access failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Adicionar aos favoritos")
                        .icon(IconName::Star)
                        .handler(move |_, cx| {
                            if let Err(error) = favorite.update(cx, |this, cx| {
                                this.save_current_url("momor_browser_favorites", cx);
                            }) {
                                tracing::debug!("saving browser favorite failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Salvar captura PNG")
                        .icon(IconName::Download)
                        .handler(move |_, cx| {
                            if let Err(error) = screenshot.update(cx, |this, cx| {
                                this.save_screenshot(cx);
                            }) {
                                tracing::debug!("saving browser screenshot failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Salvar como PDF")
                        .icon(IconName::FileDoc)
                        .handler(move |_, cx| {
                            if let Err(error) = pdf.update(cx, |this, cx| {
                                this.save_pdf(cx);
                            }) {
                                tracing::debug!("saving browser PDF failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Imprimir...")
                        .icon(IconName::FileDoc)
                        .handler(move |_, cx| {
                            if let Err(error) = print.update(cx, |this, cx| {
                                this.save_pdf(cx);
                            }) {
                                tracing::debug!("printing browser page failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Transmitir...")
                        .icon(IconName::Screen)
                        .disabled(true),
                )
                .item(
                    ContextMenuEntry::new("Forçar página escura")
                        .icon(IconName::Eye)
                        .handler(move |_, cx| {
                            if let Err(error) = dark.update(cx, |this, cx| {
                                this.toggle_dark_page(cx);
                            }) {
                                tracing::debug!("toggling dark page failed: {error:#}");
                            }
                        }),
                )
                .separator()
                .item(
                    ContextMenuEntry::new("Diminuir viewport")
                        .icon(IconName::Minimize)
                        .handler(move |_window, cx| {
                            if let Err(error) = scale_down.update_in(cx, |this, window, cx| {
                                this.set_viewport_scale(-0.1, window, cx);
                            }) {
                                tracing::debug!("decreasing browser viewport failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Viewport 100%")
                        .icon(IconName::Maximize)
                        .handler(move |_window, cx| {
                            if let Err(error) = scale_reset.update_in(cx, |this, window, cx| {
                                this.set_viewport_scale(1.0, window, cx);
                            }) {
                                tracing::debug!("resetting browser viewport failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Aumentar viewport")
                        .icon(IconName::MaximizeAlt)
                        .handler(move |_window, cx| {
                            if let Err(error) = scale_up.update_in(cx, |this, window, cx| {
                                this.set_viewport_scale(0.1, window, cx);
                            }) {
                                tracing::debug!("increasing browser viewport failed: {error:#}");
                            }
                        }),
                )
                .separator()
                .item(
                    ContextMenuEntry::new("Ver código-fonte")
                        .icon(IconName::Code)
                        .handler(move |_, cx| {
                            if let Err(error) = source.update(cx, |this, cx| {
                                this.open_devtools_tab(DevtoolsTab::Dom, cx);
                                this.refresh_dom(cx);
                            }) {
                                tracing::debug!("opening browser source failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Inspecionar elemento")
                        .icon(IconName::Crosshair)
                        .handler(move |_, cx| {
                            if let Err(error) = inspect.update(cx, |this, cx| {
                                this.open_devtools_tab(DevtoolsTab::Dom, cx);
                                this.inspect_element(inspection_position, cx);
                            }) {
                                tracing::debug!("inspecting browser element failed: {error:#}");
                            }
                        }),
                )
                .item(
                    ContextMenuEntry::new("Ferramentas do desenvolvedor")
                        .icon(IconName::Debug)
                        .handler(move |_, cx| {
                            if let Err(error) = devtools.update(cx, |this, cx| {
                                this.devtools_open = true;
                                this.push_devtools_output(
                                    "DevTools conectado ao alvo Obscura atual.",
                                    cx,
                                );
                            }) {
                                tracing::debug!("opening browser devtools failed: {error:#}");
                            }
                        }),
                )
        });
        window.focus(&menu.focus_handle(cx), cx);
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
                if this
                    .context_menu
                    .as_ref()
                    .is_some_and(|(menu, _, _)| menu.focus_handle(cx).contains_focused(window, cx))
                {
                    cx.focus_self(window);
                }
                this.context_menu.take();
                cx.notify();
            });
        self.context_menu = Some((menu, position, subscription));
    }

    fn save_screenshot(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        let tab_index = self.active_tab;
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            let bytes = client.capture_screenshot(&page).await?;
            let path =
                browser_download_dir()?.join(format!("momor-page-{}.png", uuid::Uuid::new_v4()));
            tokio::fs::write(&path, bytes)
                .await
                .with_context(|| format!("failed to save screenshot to {}", path.display()))?;
            anyhow::Ok(path)
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(path) => {
                if let Err(error) = this.update(cx, |this, cx| {
                    if let Some(tab) = this.tabs.get_mut(tab_index) {
                        tab.status = format!("Captura salva em {}", path.display());
                    }
                    cx.notify();
                }) {
                    tracing::debug!("browser screenshot status update failed: {error:#}");
                }
            }
            Err(error) => {
                tracing::debug!("browser screenshot task failed: {error:#}");
            }
        })
        .detach();
    }

    fn save_pdf(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        let tab_index = self.active_tab;
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            let bytes = client.print_to_pdf(&page).await?;
            let path =
                browser_download_dir()?.join(format!("momor-page-{}.pdf", uuid::Uuid::new_v4()));
            tokio::fs::write(&path, bytes)
                .await
                .with_context(|| format!("failed to save PDF to {}", path.display()))?;
            anyhow::Ok(path)
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(path) => {
                if let Err(error) = this.update(cx, |this, cx| {
                    if let Some(tab) = this.tabs.get_mut(tab_index) {
                        tab.status = format!("PDF salvo em {}", path.display());
                    }
                    cx.notify();
                }) {
                    tracing::debug!("browser PDF status update failed: {error:#}");
                }
            }
            Err(error) => tracing::debug!("browser PDF task failed: {error:#}"),
        })
        .detach();
    }

    fn toggle_dark_page(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        let tab_index = self.active_tab;
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            client
                .evaluate(
                    &page,
                    r#"(() => { const id = '__momor_dark_mode'; const old = document.getElementById(id); if (old) { old.remove(); return 'off'; } const style = document.createElement('style'); style.id = id; style.textContent = 'html { filter: invert(0.92) hue-rotate(180deg) !important; background: #111 !important; } img, video, canvas, iframe { filter: invert(1) hue-rotate(180deg) !important; }'; document.documentElement.appendChild(style); return 'on'; })()"#,
                )
                .await
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(value) => {
                if let Err(error) = this.update(cx, |this, cx| {
                    if let Some(tab) = this.tabs.get_mut(tab_index) {
                        tab.status = format!("Modo escuro: {}", value.as_str().unwrap_or("ok"));
                    }
                    cx.notify();
                }) {
                    tracing::debug!("dark page status update failed: {error:#}");
                }
            }
            Err(error) => tracing::debug!("dark page task failed: {error:#}"),
        })
        .detach();
    }

    fn set_viewport_scale(&mut self, change: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.viewport_auto = false;
        self.viewport_scale = if (change - 1.0).abs() < f32::EPSILON {
            1.0
        } else {
            (self.viewport_scale + change).clamp(0.5, 2.0)
        };
        let Some(bounds) = self.content_bounds else {
            return;
        };
        let tab_index = self.active_tab;
        let (Some(client), Some(page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        let width = (f32::from(bounds.size.width) / self.viewport_scale)
            .round()
            .max(1.0) as u32;
        let height = (f32::from(bounds.size.height) / self.viewport_scale)
            .round()
            .max(1.0) as u32;
        self.viewport_size = Some((width, height));
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            client.set_viewport(&page, width, height).await
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = task.await {
                tracing::debug!("browser viewport task failed: {error:#}");
            }
            if let Err(error) = this.update(cx, |this, cx| {
                if let Some(tab) = this.tabs.get_mut(tab_index) {
                    tab.status = format!("Viewport {}%", (this.viewport_scale * 100.0).round());
                }
                cx.notify();
            }) {
                tracing::debug!("browser viewport status update failed: {error:#}");
            }
        })
        .detach();
    }

    fn fit_viewport(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.viewport_auto = true;
        self.viewport_scale = 1.0;
        self.viewport_size = None;
        if let Some(bounds) = self.content_bounds {
            self.sync_auto_viewport(bounds, window, cx);
        }
        cx.notify();
    }

    fn sync_auto_viewport(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.viewport_auto {
            return;
        }
        let width = f32::from(bounds.size.width).round().max(1.0) as u32;
        let height = f32::from(bounds.size.height).round().max(1.0) as u32;
        if self.viewport_size == Some((width, height)) {
            return;
        }
        self.viewport_size = Some((width, height));
        let (Some(client), Some(page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            client.set_viewport(&page, width, height).await
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = task.await {
                tracing::debug!("automatic browser viewport update failed: {error:#}");
            }
            if let Err(error) = this.update(cx, |_this, cx| cx.notify()) {
                tracing::debug!("automatic browser viewport repaint failed: {error:#}");
            }
        })
        .detach();
    }

    fn open_devtools_tab(&mut self, tab: DevtoolsTab, cx: &mut Context<Self>) {
        #[cfg(target_os = "windows")]
        if let Some(native_browser) = &self.native_browser {
            if let Err(error) = native_browser.open_devtools(self.active_tab) {
                if let Some(active_tab) = self.active_tab_mut() {
                    active_tab.status = format!("DevTools indisponível: {error:#}");
                }
            }
            self.devtools_open = false;
            cx.notify();
            return;
        }
        self.devtools_open = true;
        self.devtools_tab = tab;
        if tab == DevtoolsTab::Dom {
            self.refresh_dom(cx);
        }
        cx.notify();
    }

    fn close_devtools(&mut self, cx: &mut Context<Self>) {
        self.devtools_open = false;
        cx.notify();
    }

    fn visible_devtools_output(&self) -> Vec<String> {
        self.devtools_output
            .iter()
            .filter(|line| match self.devtools_tab {
                DevtoolsTab::Console => line.starts_with('[') || line.starts_with("Erro"),
                DevtoolsTab::Network => {
                    line.starts_with("-> ") || line.starts_with("Navegou para ")
                }
                DevtoolsTab::Dom => {
                    !line.starts_with('[')
                        && !line.starts_with("-> ")
                        && !line.starts_with("Navegou para ")
                }
            })
            .cloned()
            .collect()
    }

    fn refresh_dom(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            client
                .evaluate(
                    &page,
                    "document.documentElement ? document.documentElement.outerHTML : ''",
                )
                .await
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(value) => {
                if let Err(error) = this.update(cx, |this, cx| {
                    this.push_devtools_output(value.as_str().unwrap_or("<empty>"), cx);
                }) {
                    tracing::debug!("DOM DevTools update failed: {error:#}");
                }
            }
            Err(error) => tracing::debug!("DOM DevTools request failed: {error:#}"),
        })
        .detach();
    }

    fn inspect_element(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((x, y)) = self.browser_coordinates(position) else {
            self.push_devtools_output("Ponto fora da página.", cx);
            return;
        };
        let (Some(client), Some(page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        let expression = format!(
            "(() => {{ const el = document.elementFromPoint({x}, {y}); if (!el) return {{ error: 'Nenhum elemento' }}; return {{ tag: el.tagName, id: el.id || '', classes: el.className || '', text: (el.innerText || el.textContent || '').slice(0, 500), html: el.outerHTML.slice(0, 2000) }}; }})()"
        );
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            client.evaluate(&page, &expression).await
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(value) => {
                let output =
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                if let Err(error) = this.update(cx, |this, cx| {
                    this.push_devtools_output(output, cx);
                }) {
                    tracing::debug!("element inspection update failed: {error:#}");
                }
            }
            Err(error) => tracing::debug!("element inspection failed: {error:#}"),
        })
        .detach();
    }

    fn dispatch_mouse_wheel(
        &self,
        position: Point<Pixels>,
        delta_x: f32,
        delta_y: f32,
        cx: &mut Context<Self>,
    ) {
        #[cfg(target_os = "windows")]
        if self.native_browser.is_some() {
            return;
        }
        let (Some(client), Some(page), Some((x, y))) = (
            self.client.clone(),
            self.active_page(),
            self.browser_coordinates(position),
        ) else {
            return;
        };
        gpui_tokio::Tokio::handle(cx).spawn(async move {
            if let Err(error) = client
                .dispatch_mouse_wheel(&page, x, y, delta_x, delta_y)
                .await
            {
                tracing::debug!("browser wheel event failed: {error:#}");
            }
        });
    }

    fn dispatch_key(
        &self,
        event_type: &'static str,
        key: String,
        text: String,
        modifiers: u8,
        cx: &mut Context<Self>,
    ) {
        #[cfg(target_os = "windows")]
        if self.native_browser.is_some() {
            return;
        }
        let (Some(client), Some(page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        gpui_tokio::Tokio::handle(cx).spawn(async move {
            if let Err(error) = client
                .dispatch_key_event(&page, event_type, &key, &text, modifiers)
                .await
            {
                tracing::debug!("browser key event failed: {error:#}");
            }
        });
    }

    fn navigate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(address_bar) = &self.address_bar else {
            return;
        };
        let address = address_bar.read(cx).text(cx);
        let url = match normalize_url(&address) {
            Ok(url) => url,
            Err(error) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.status = error.to_string();
                }
                cx.notify();
                return;
            }
        };
        let tab_index = self.active_tab;
        let endpoint = default_endpoint();
        #[cfg(target_os = "windows")]
        let chromium_port = self.native_browser.as_ref().map(NativeBrowser::debug_port);
        #[cfg(not(target_os = "windows"))]
        let chromium_port = None;
        let existing_client = self.client.clone();
        let existing_page = self.active_page();
        let use_existing_chromium_page = chromium_port.is_some()
            && tab_index == 0
            && self.tabs.len() == 1
            && existing_client.is_none()
            && existing_page.is_none();
        let should_start_screencast = chromium_port.is_none()
            && existing_page
                .as_ref()
                .map(|page| !self.screencast_tasks.contains_key(&page.target_id))
                .unwrap_or(true);
        let viewport = self.content_bounds.map(|bounds| {
            (
                f32::from(bounds.size.width).round().max(1.0) as u32,
                f32::from(bounds.size.height).round().max(1.0) as u32,
            )
        });
        if let Some(tab) = self.active_tab_mut() {
            tab.status = if existing_page.is_some() {
                "Navegando...".to_string()
            } else if chromium_port.is_some() {
                "Conectando ao Chromium...".to_string()
            } else {
                format!("Conectando ao Obscura em {endpoint}...")
            };
            tab.image = None;
            tab.loading = true;
        }
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            let client = match (existing_client, chromium_port) {
                (Some(client), _) => client,
                (None, Some(port)) => CdpClient::connect_chromium(port).await?,
                (None, None) => match CdpClient::connect(&endpoint).await {
                    Ok(client) => client,
                    Err(connect_error) => CdpClient::launch_and_connect()
                        .await
                        .with_context(|| format!("{connect_error:#}"))?,
                },
            };
            let events = client.subscribe();
            let download_dir = browser_download_dir()?;
            client.set_download_behavior(&download_dir).await?;
            let page = match existing_page {
                Some(page) => {
                    let mut page = page;
                    client.navigate(&mut page, &url).await?;
                    page
                }
                None if use_existing_chromium_page => client.connect_existing_page(&url).await?,
                None => client.create_page(&url).await?,
            };
            if let Some((width, height)) = viewport {
                client.set_viewport(&page, width, height).await?;
            }
            let screenshot = match client.capture_screenshot(&page).await {
                Ok(screenshot) => screenshot,
                Err(initial_error) => {
                    client.settle_page(&page).await.with_context(|| {
                        format!("initial browser screenshot failed: {initial_error:#}")
                    })?;
                    client.capture_screenshot(&page).await?
                }
            };
            let screencast = if should_start_screencast {
                match client.start_screencast(&page).await {
                    Ok(events) => Some(events),
                    Err(error) => {
                        tracing::debug!("failed to start browser screencast: {error:#}");
                        None
                    }
                }
            } else {
                None
            };
            Ok(BrowserNavigation {
                client,
                page,
                screenshot,
                screencast,
                events,
            })
        });
        self.navigation_task = Some(cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(navigation) => {
                if let Err(error) = this.update_in(cx, |this, window, cx| {
                    let client = navigation.client.clone();
                    let page = navigation.page.clone();
                    this.client = Some(client.clone());
                    if let Some(tab) = this.tabs.get_mut(tab_index) {
                        tab.page = Some(page.clone());
                        tab.title = page.url.clone();
                        tab.image = Some(Arc::new(Image::from_bytes(
                            ImageFormat::Png,
                            navigation.screenshot,
                        )));
                        tab.status = page.url.clone();
                        tab.loading = false;
                    }
                    if this.active_tab == tab_index {
                        this.sync_address_bar(window, cx);
                        this.sync_history(tab_index, window, cx);
                    }
                    if let Some(events) = navigation.screencast {
                        this.start_screencast(client, page, events, window, cx);
                    }
                    if this.download_events_task.is_none() {
                        this.start_download_events(navigation.events, window, cx);
                    }
                    cx.notify();
                }) {
                    tracing::debug!("browser navigation UI update failed: {error:#}");
                }
            }
            Err(error) => {
                if let Err(update_error) = this.update(cx, |this, cx| {
                    if let Some(tab) = this.tabs.get_mut(tab_index) {
                        tab.status = format!("Falha no navegador: {error:#}");
                    }
                    cx.notify();
                }) {
                    tracing::debug!("browser navigation error UI update failed: {update_error:#}");
                }
            }
        }));
    }

    fn start_screencast(
        &mut self,
        client: CdpClient,
        page: BrowserPage,
        mut events: broadcast::Receiver<CdpEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target_id = page.target_id.clone();
        let task_target_id = target_id.clone();
        let session_id = page.session_id.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let mut last_frame_at = Instant::now() - Duration::from_millis(32);
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if event.method != "Page.screencastFrame"
                    || event.session_id.as_deref() != Some(&session_id)
                {
                    continue;
                }
                let Some(encoded) = event.params.get("data").and_then(Value::as_str) else {
                    continue;
                };
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
                    continue;
                };
                if let Some(frame_id) = event.params.get("sessionId").and_then(Value::as_i64)
                    && let Err(error) = client.acknowledge_screencast_frame(&page, frame_id).await
                {
                    tracing::debug!("browser screencast acknowledgement failed: {error:#}");
                }
                if last_frame_at.elapsed() < Duration::from_millis(16) {
                    continue;
                }
                last_frame_at = Instant::now();
                if let Err(error) = this.update(cx, |this, cx| {
                    if let Some(tab) = this.tabs.iter_mut().find(|tab| {
                        tab.page
                            .as_ref()
                            .is_some_and(|page| page.target_id == task_target_id)
                    }) {
                        tab.image = Some(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)));
                        tab.status = tab
                            .page
                            .as_ref()
                            .map(|page| page.url.clone())
                            .unwrap_or_else(|| "Carregando...".to_string());
                        cx.notify();
                    }
                }) {
                    tracing::debug!("browser screencast UI update failed: {error:#}");
                    break;
                }
            }
        });
        self.screencast_tasks.insert(target_id, task);
    }

    fn start_download_events(
        &mut self,
        mut events: broadcast::Receiver<CdpEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.download_events_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let status = match event.method.as_str() {
                    "Browser.downloadWillBegin" => event
                        .params
                        .get("suggestedFilename")
                        .and_then(Value::as_str)
                        .map(|filename| format!("Baixando {filename}...")),
                    "Browser.downloadProgress" => {
                        match event.params.get("state").and_then(Value::as_str) {
                            Some("completed") => event
                                .params
                                .get("suggestedFilename")
                                .and_then(Value::as_str)
                                .map(|filename| format!("Download concluído: {filename}")),
                            Some("canceled") | Some("interrupted") => {
                                Some("Download interrompido".to_string())
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let devtools_line = match event.method.as_str() {
                    "Runtime.consoleAPICalled" => {
                        let level = event
                            .params
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("log");
                        let text = event
                            .params
                            .get("args")
                            .and_then(Value::as_array)
                            .map(|args| {
                                args.iter()
                                    .filter_map(|arg| {
                                        arg.get("value").and_then(Value::as_str).or_else(|| {
                                            arg.get("description").and_then(Value::as_str)
                                        })
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .unwrap_or_default();
                        Some(format!("[{level}] {text}"))
                    }
                    "Network.requestWillBeSent" => event
                        .params
                        .get("request")
                        .and_then(|request| request.get("url"))
                        .and_then(Value::as_str)
                        .map(|url| format!("-> {url}")),
                    "Page.frameNavigated" => event
                        .params
                        .get("frame")
                        .and_then(|frame| frame.get("url"))
                        .and_then(Value::as_str)
                        .map(|url| format!("Navegou para {url}")),
                    _ => None,
                };
                if status.is_none() && devtools_line.is_none() {
                    continue;
                }
                let is_main_frame = event
                    .params
                    .get("frame")
                    .and_then(|frame| frame.get("parentId"))
                    .is_none();
                let navigated_url = (event.method == "Page.frameNavigated" && is_main_frame)
                    .then(|| {
                        event
                            .params
                            .get("frame")
                            .and_then(|frame| frame.get("url"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten();
                let event_session = event.session_id.clone();
                if let Err(error) = this.update_in(cx, |this, window, cx| {
                    if let Some(status) = status {
                        this.download_status = Some(status);
                    }
                    if let Some(line) = devtools_line {
                        this.push_devtools_output(line, cx);
                    }
                    if let Some(url) = navigated_url {
                        if let Some((index, tab)) =
                            this.tabs.iter_mut().enumerate().find(|(_, tab)| {
                                tab.page.as_ref().is_some_and(|page| {
                                    Some(&page.session_id) == event_session.as_ref()
                                })
                            })
                        {
                            if let Some(page) = tab.page.as_mut() {
                                page.url = url.clone();
                            }
                            tab.title = url;
                            tab.loading = false;
                            if this.active_tab == index {
                                this.sync_address_bar(window, cx);
                                this.sync_history(index, window, cx);
                            }
                        }
                    }
                    cx.notify();
                }) {
                    tracing::debug!("browser download UI update failed: {error:#}");
                    break;
                }
            }
        }));
    }

    fn navigate_history(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let tab_index = self.active_tab;
        let (Some(client), Some(mut page)) = (self.client.clone(), self.active_page()) else {
            return;
        };
        if let Some(tab) = self.active_tab_mut() {
            tab.status = if delta < 0 {
                "Voltando...".to_string()
            } else {
                "Avançando...".to_string()
            };
            tab.image = None;
            tab.loading = true;
        }
        self.navigation_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                client.navigate_history(&mut page, delta).await?;
                let screenshot = match client.capture_screenshot(&page).await {
                    Ok(screenshot) => screenshot,
                    Err(initial_error) => {
                        client.settle_page(&page).await.with_context(|| {
                            format!("history screenshot failed: {initial_error:#}")
                        })?;
                        client.capture_screenshot(&page).await?
                    }
                };
                anyhow::Ok((page, screenshot))
            }
            .await;
            if let Err(error) = this.update_in(cx, |this, window, cx| {
                if let Some(tab) = this.tabs.get_mut(tab_index) {
                    match result {
                        Ok((page, screenshot)) => {
                            tab.page = Some(page.clone());
                            tab.image =
                                Some(Arc::new(Image::from_bytes(ImageFormat::Png, screenshot)));
                            tab.status = page.url.clone();
                            tab.loading = false;
                            if this.active_tab == tab_index {
                                this.sync_address_bar(window, cx);
                                this.sync_history(tab_index, window, cx);
                            }
                        }
                        Err(error) => tab.status = format!("Falha no histórico: {error:#}"),
                    }
                }
                cx.notify();
            }) {
                tracing::debug!("browser history UI update failed: {error:#}");
            }
        }));
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab_index = self.active_tab;
        let (Some(client), Some(mut page)) = (self.client.clone(), self.active_page()) else {
            self.navigate(window, cx);
            return;
        };
        if let Some(tab) = self.active_tab_mut() {
            tab.status = "Atualizando...".to_string();
            tab.image = None;
            tab.loading = true;
        }
        self.navigation_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let url = page.url.clone();
                client.navigate(&mut page, &url).await?;
                let screenshot = match client.capture_screenshot(&page).await {
                    Ok(screenshot) => screenshot,
                    Err(initial_error) => {
                        client.settle_page(&page).await.with_context(|| {
                            format!("reload screenshot failed: {initial_error:#}")
                        })?;
                        client.capture_screenshot(&page).await?
                    }
                };
                anyhow::Ok((page, screenshot))
            }
            .await;
            if let Err(error) = this.update_in(cx, |this, window, cx| {
                if let Some(tab) = this.tabs.get_mut(tab_index) {
                    match result {
                        Ok((page, screenshot)) => {
                            tab.page = Some(page.clone());
                            tab.image =
                                Some(Arc::new(Image::from_bytes(ImageFormat::Png, screenshot)));
                            tab.status = page.url.clone();
                            tab.loading = false;
                            if this.active_tab == tab_index {
                                this.sync_address_bar(window, cx);
                                this.sync_history(tab_index, window, cx);
                            }
                        }
                        Err(error) => tab.status = format!("Falha ao atualizar: {error:#}"),
                    }
                }
                cx.notify();
            }) {
                tracing::debug!("browser reload UI update failed: {error:#}");
            }
        }));
    }
}

impl Focusable for BrowserPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<()> for BrowserPanel {}

impl Render for BrowserPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_address_bar(window, cx);
        let address_bar = self.address_bar.clone();
        let (status, image) = self
            .active_tab()
            .map(|tab| (SharedString::from(tab.status.clone()), tab.image.clone()))
            .unwrap_or_else(|| (SharedString::from("Nenhuma aba aberta"), None));
        let download_status = self.download_status.clone().map(SharedString::from);
        let content_bounds_entity = cx.weak_entity();
        let active_tab = self.active_tab;
        let devtools_open = self.devtools_open;
        let devtools_tab = self.devtools_tab;
        let history_open = self.history_open;
        let history_entries = self
            .active_tab()
            .map(|tab| (tab.history.clone(), tab.history_index))
            .unwrap_or_default();
        let favorite_icon = if self.favorites.iter().any(|url| url == &self.active_url()) {
            IconName::StarFilled
        } else {
            IconName::Star
        };
        #[cfg(target_os = "windows")]
        let native_browser_active = self.native_browser.is_some();
        #[cfg(not(target_os = "windows"))]
        let native_browser_active = false;
        let tab_labels = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let label = tab.label();
                let active = index == active_tab;
                let drag = notebook::DraggedBrowserTab {
                    url: tab
                        .page
                        .as_ref()
                        .map(|page| page.url.clone())
                        .unwrap_or_else(|| tab.current_url.clone()),
                    title: tab.title.clone(),
                };
                h_flex()
                    .id(("browser-tab", index))
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(if active {
                        cx.theme().colors().text
                    } else {
                        cx.theme().colors().border
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_tab(index, window, cx);
                    }))
                    .on_drag(drag, |tab, _, _, cx| cx.new(|_| tab.clone()))
                    .child(Label::new(label))
                    .child(
                        IconButton::new(("browser-close-tab", index), IconName::Close)
                            .tooltip(Tooltip::text("Fechar aba"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.close_tab(index, cx);
                            })),
                    )
            })
            .collect::<Vec<_>>();
        v_flex()
            .key_context("BrowserPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .bg(cx.theme().colors().panel_background)
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .px_2()
                    .pt_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .children(tab_labels)
                    .child(
                        IconButton::new("browser-new-tab", IconName::Plus)
                            .tooltip(Tooltip::text("Nova aba"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_tab(window, cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        let address_bar_focused =
                            this.address_bar.as_ref().is_some_and(|address_bar| {
                                address_bar
                                    .read(cx)
                                    .focus_handle(cx)
                                    .contains_focused(window, cx)
                            });
                        if address_bar_focused && event.keystroke.key.eq_ignore_ascii_case("enter")
                        {
                            this.navigate(window, cx);
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        IconButton::new("browser-back", IconName::ArrowLeft)
                            .tooltip(Tooltip::text("Voltar"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.navigate_history(-1, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("browser-forward", IconName::ArrowRight)
                            .tooltip(Tooltip::text("Avançar"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.navigate_history(1, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("browser-reload", IconName::RotateCw)
                            .tooltip(Tooltip::text("Atualizar"))
                            .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
                    )
                    .child(
                        IconButton::new("browser-history", IconName::HistoryRerun)
                            .tooltip(Tooltip::text("Histórico"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_history(window, cx);
                            })),
                    )
                    .when_some(address_bar, |this, address_bar| {
                        this.child(div().flex_1().min_w_0().child(address_bar))
                    })
                    .when_some(download_status, |this, status| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(Icon::new(IconName::Download).size(IconSize::Small))
                                .child(Label::new(status).color(Color::Muted)),
                        )
                    })
                    .child(
                        IconButton::new("browser-go", IconName::ArrowRight)
                            .tooltip(Tooltip::text("Navegar"))
                            .on_click(cx.listener(|this, _, window, cx| this.navigate(window, cx))),
                    )
                    .child(
                        IconButton::new("browser-favorite", favorite_icon)
                            .tooltip(Tooltip::text("Adicionar ou remover dos favoritos"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_favorite(cx);
                            })),
                    ),
            )
            .when(history_open, |this| {
                let (entries, history_index) = history_entries.clone();
                let rows = entries.into_iter().enumerate().map(|(index, entry)| {
                    let selected = index == history_index;
                    let selected_entry = entry.clone();
                    h_flex()
                        .id(("browser-history-entry", index))
                        .w_full()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .when(selected, |row| {
                            row.bg(cx.theme().colors().element_background)
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.navigate_history_entry(selected_entry.clone(), window, cx);
                        }))
                        .child(
                            Icon::new(if selected {
                                IconName::ArrowRight
                            } else {
                                IconName::HistoryRerun
                            })
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(Label::new(entry.title).size(LabelSize::Small))
                                .child(
                                    Label::new(entry.url)
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                ),
                        )
                });
                this.child(
                    v_flex()
                        .id("browser-history-panel")
                        .max_h(px(180.))
                        .w_full()
                        .flex_none()
                        .overflow_y_scroll()
                        .border_b_1()
                        .border_color(cx.theme().colors().border)
                        .children(rows),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .on_children_prepainted(move |children, window, cx| {
                        if let Some(bounds) = children.first().copied() {
                            if let Err(error) = content_bounds_entity.update(cx, |this, _cx| {
                                this.content_bounds = Some(bounds);
                            }) {
                                tracing::debug!("browser content bounds update failed: {error:#}");
                            } else if let Err(error) =
                                content_bounds_entity.update(cx, |this, cx| {
                                    #[cfg(target_os = "windows")]
                                    this.ensure_native_browser(bounds, window, cx);
                                    this.sync_auto_viewport(bounds, window, cx);
                                })
                            {
                                tracing::debug!(
                                    "browser automatic viewport sync failed: {error:#}"
                                );
                            }
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                            this.dispatch_mouse(
                                "mousePressed",
                                event.position,
                                event.click_count,
                                "left",
                                cdp_modifiers(&event.modifiers),
                                cx,
                            );
                            cx.focus_self(window);
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseUpEvent, _window, cx| {
                            this.dispatch_mouse(
                                "mouseReleased",
                                event.position,
                                event.click_count,
                                "left",
                                cdp_modifiers(&event.modifiers),
                                cx,
                            );
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.open_context_menu(event.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                        this.dispatch_mouse(
                            "mouseMoved",
                            event.position,
                            0,
                            "none",
                            cdp_modifiers(&event.modifiers),
                            cx,
                        );
                    }))
                    .on_scroll_wheel(cx.listener(
                        |this, event: &gpui::ScrollWheelEvent, _window, cx| {
                            let delta = event.delta.pixel_delta(px(16.0));
                            this.dispatch_mouse_wheel(
                                event.position,
                                f32::from(delta.x),
                                f32::from(delta.y),
                                cx,
                            );
                            cx.stop_propagation();
                        },
                    ))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        if this.address_bar.as_ref().is_some_and(|address_bar| {
                            address_bar
                                .read(cx)
                                .focus_handle(cx)
                                .contains_focused(window, cx)
                        }) {
                            return;
                        }
                        let key = event.keystroke.key.to_ascii_lowercase();
                        let command =
                            event.keystroke.modifiers.control || event.keystroke.modifiers.platform;
                        if command && key == "l" {
                            if let Some(address_bar) = this.address_bar.clone() {
                                window.focus(&address_bar.read(cx).focus_handle(cx), cx);
                            }
                        } else if command && key == "t" {
                            this.add_tab(window, cx);
                        } else if command && key == "w" {
                            this.close_tab(this.active_tab, cx);
                        } else if command && key == "r" {
                            this.reload(window, cx);
                        } else if event.keystroke.modifiers.alt && key == "left" {
                            this.navigate_history(-1, window, cx);
                        } else if event.keystroke.modifiers.alt && key == "right" {
                            this.navigate_history(1, window, cx);
                        } else {
                            this.dispatch_key(
                                "keyDown",
                                event.keystroke.key.clone(),
                                event.keystroke.key_char.clone().unwrap_or_default(),
                                cdp_modifiers(&event.keystroke.modifiers),
                                cx,
                            );
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                    }))
                    .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, window, cx| {
                        if this.address_bar.as_ref().is_some_and(|address_bar| {
                            address_bar
                                .read(cx)
                                .focus_handle(cx)
                                .contains_focused(window, cx)
                        }) {
                            return;
                        }
                        this.dispatch_key(
                            "keyUp",
                            event.keystroke.key.clone(),
                            String::new(),
                            cdp_modifiers(&event.keystroke.modifiers),
                            cx,
                        );
                        window.prevent_default();
                        cx.stop_propagation();
                    }))
                    .when(!native_browser_active, |this| {
                        this.when_some(image.clone(), |this, image| {
                            this.child(img(image).size_full())
                        })
                    })
                    .when(!native_browser_active && image.is_none(), |this| {
                        this.child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .child(Icon::new(IconName::Public).size(IconSize::XLarge))
                                .child(Label::new(status).color(Color::Muted)),
                        )
                    })
                    .when(native_browser_active, |this| {
                        // Keep a layout child so the prepaint callback continues to receive
                        // live content bounds after Chromium takes over rendering.
                        this.child(div().size_full())
                    }),
            )
            .when(devtools_open, |_| {
                let tabs = [DevtoolsTab::Console, DevtoolsTab::Network, DevtoolsTab::Dom]
                    .into_iter()
                    .map(|tab| {
                        let active = tab == devtools_tab;
                        h_flex()
                            .id(SharedString::from(format!(
                                "browser-devtools-{}",
                                tab.label()
                            )))
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(if active {
                                cx.theme().colors().text
                            } else {
                                cx.theme().colors().border
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_devtools_tab(tab, cx);
                            }))
                            .child(Label::new(tab.label()))
                    })
                    .collect::<Vec<_>>();
                let output = self.visible_devtools_output();
                let output = output.into_iter().map(|line| {
                    Label::new(line)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .into_any_element()
                });
                v_flex()
                    .h(px(220.))
                    .w_full()
                    .flex_none()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().editor_background)
                    .child(
                        h_flex()
                            .gap_1()
                            .px_2()
                            .items_center()
                            .children(tabs)
                            .child(div().flex_1())
                            .child(
                                IconButton::new("browser-devtools-close", IconName::Close)
                                    .tooltip(Tooltip::text("Fechar DevTools"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_devtools(cx);
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .id("browser-devtools-output")
                            .flex_1()
                            .overflow_y_scroll()
                            .px_2()
                            .py_1()
                            .gap_0p5()
                            .children(output),
                    )
            })
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(gpui::Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{ObscuraReadyFile, normalize_url};

    #[test]
    fn normalizes_hostnames_for_the_address_bar() {
        assert_eq!(
            normalize_url("example.com").unwrap(),
            "https://example.com/"
        );
        assert_eq!(normalize_url("about:blank").unwrap(), "about:blank");
    }

    #[test]
    fn sends_plain_text_to_google_search() {
        assert_eq!(
            normalize_url("rust gpui").unwrap(),
            "https://www.google.com/search?q=rust+gpui"
        );
    }

    #[test]
    fn rejects_unsupported_schemes() {
        let error = normalize_url("file:///etc/passwd").unwrap_err().to_string();
        assert!(error.contains("does not allow"));
    }

    #[test]
    fn formats_the_ready_file_endpoint() {
        let ready: ObscuraReadyFile = serde_json::from_str(
            r#"{"host":"127.0.0.1","port":43127,"websocket_path":"/devtools/browser"}"#,
        )
        .unwrap();
        assert_eq!(
            format!("ws://{}:{}{}", ready.host, ready.port, ready.websocket_path),
            "ws://127.0.0.1:43127/devtools/browser"
        );
    }
}
