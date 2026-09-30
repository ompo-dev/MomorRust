//! Momor's first-party browser surface.
//!
//! The browser engine stays in Obscura. Momor owns the user-facing surface and
//! speaks the Chrome DevTools Protocol over a local WebSocket. Keeping this
//! boundary explicit lets us replace screenshot polling with screencast frames
//! later without coupling the two applications' renderers.

use anyhow::{Context as _, Result, anyhow};
use async_tungstenite::tungstenite::Message;
use base64::Engine as _;
use futures::StreamExt;
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Image, ImageFormat,
    MouseButton, Pixels, Point, Render, SharedString, Subscription, Task, WeakEntity, Window, img,
    prelude::*,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::{broadcast, mpsc, oneshot},
    time::sleep,
};
use ui::{IconName, Label, Tooltip, prelude::*};
use ui_input::InputField;
use url::Url;
use workspace::Workspace;

const DEFAULT_CDP_ENDPOINT: &str = "ws://127.0.0.1:9222/devtools/browser";

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
    _obscura_process: Option<Arc<ObscuraProcess>>,
}

impl CdpClient {
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let endpoint_url = Url::parse(endpoint).context("invalid Obscura CDP endpoint")?;
        if !matches!(endpoint_url.scheme(), "ws" | "wss") {
            anyhow::bail!("Obscura CDP endpoint must use ws:// or wss://");
        }

        let (socket, _) = async_tungstenite::tokio::connect_async(endpoint)
            .await
            .context("failed to connect to Obscura CDP")?;
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(64);
        let client = Self {
            commands,
            events: events.clone(),
            next_id: Arc::new(AtomicU64::new(1)),
            _obscura_process: None,
        };

        tokio::spawn(run_cdp_socket(socket, command_rx, events));
        Ok(client)
    }

    async fn launch_and_connect() -> Result<Self> {
        let binary = std::env::var_os("MOMOR_OBSCURA_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("obscura"));
        let ready_path = std::env::temp_dir().join(format!(
            "momor-obscura-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut child = Command::new(&binary)
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "--workers",
                "1",
                "--max-connections",
                "4",
                "--ready-file",
            ])
            .arg(&ready_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| {
                format!(
                    "failed to start Obscura; set MOMOR_OBSCURA_BINARY to the rendered obscura executable (attempted {})",
                    binary.display()
                )
            })?;

        let ready = wait_for_ready_file(&mut child, &ready_path).await;
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                let _ = child.start_kill();
                let _ = std::fs::remove_file(&ready_path);
                return Err(error);
            }
        };
        let endpoint = format!("ws://{}:{}{}", ready.host, ready.port, ready.websocket_path);
        let process = Arc::new(ObscuraProcess {
            child: std::sync::Mutex::new(child),
            ready_path,
        });
        let mut client = Self::connect(&endpoint).await?;
        client._obscura_process = Some(process);
        Ok(client)
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
            .send("Target.createTarget", json!({ "url": url }))
            .await?;
        let target_id = created
            .get("targetId")
            .and_then(Value::as_str)
            .context("Obscura did not return a target id")?
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
            .context("Obscura did not return a target session id")?
            .to_string();

        self.send_with_session("Page.enable", json!({}), Some(&session_id))
            .await?;
        self.send_with_session("Runtime.enable", json!({}), Some(&session_id))
            .await?;

        Ok(BrowserPage {
            target_id,
            session_id,
            url: url.to_string(),
        })
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
    ) -> Result<()> {
        self.send_with_session(
            "Input.dispatchMouseEvent",
            json!({
                "type": event_type,
                "x": x,
                "y": y,
                "button": "left",
                "buttons": if event_type == "mousePressed" { 1 } else { 0 },
                "clickCount": click_count,
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

#[derive(Debug, Deserialize)]
struct ObscuraReadyFile {
    host: String,
    port: u16,
    websocket_path: String,
}

struct ObscuraProcess {
    child: std::sync::Mutex<Child>,
    ready_path: PathBuf,
}

impl Drop for ObscuraProcess {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.start_kill();
        }
        let _ = std::fs::remove_file(&self.ready_path);
    }
}

async fn wait_for_ready_file(
    child: &mut Child,
    ready_path: &std::path::Path,
) -> Result<ObscuraReadyFile> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(bytes) = tokio::fs::read(ready_path).await {
            return serde_json::from_slice(&bytes).context("invalid Obscura ready file");
        }
        if let Some(status) = child
            .try_wait()
            .context("failed to inspect Obscura process")?
        {
            anyhow::bail!("Obscura exited before becoming ready ({status})");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Obscura did not become ready within 20 seconds");
        }
        sleep(Duration::from_millis(25)).await;
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

struct BrowserNavigation {
    client: CdpClient,
    page: BrowserPage,
    screenshot: Vec<u8>,
    screencast: Option<broadcast::Receiver<CdpEvent>>,
}

pub struct BrowserPanel {
    focus_handle: FocusHandle,
    _workspace: WeakEntity<Workspace>,
    address_bar: Option<Entity<InputField>>,
    _address_subscription: Option<Subscription>,
    client: Option<CdpClient>,
    page: Option<BrowserPage>,
    image: Option<Arc<Image>>,
    status: SharedString,
    navigation_task: Option<Task<()>>,
    screencast_task: Option<Task<()>>,
    content_bounds: Option<Bounds<Pixels>>,
}

impl BrowserPanel {
    pub fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            _workspace: workspace,
            address_bar: None,
            _address_subscription: None,
            client: None,
            page: None,
            image: None,
            status: "Inicie o Obscura e navegue para uma URL".into(),
            navigation_task: None,
            screencast_task: None,
            content_bounds: None,
        }
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

    fn set_address_bar(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(address_bar) = self.address_bar.clone() {
            address_bar.update(cx, |field, cx| field.set_text(url, window, cx));
        }
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
        Some((f32::from(local.x), f32::from(local.y)))
    }

    fn dispatch_mouse(
        &self,
        event_type: &'static str,
        position: Point<Pixels>,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        let (Some(client), Some(page), Some((x, y))) = (
            self.client.clone(),
            self.page.clone(),
            self.browser_coordinates(position),
        ) else {
            return;
        };
        let _ = gpui_tokio::Tokio::handle(cx).spawn(async move {
            let _ = client
                .dispatch_mouse_event(&page, event_type, x, y, click_count)
                .await;
        });
    }

    fn dispatch_mouse_wheel(
        &self,
        position: Point<Pixels>,
        delta_x: f32,
        delta_y: f32,
        cx: &mut Context<Self>,
    ) {
        let (Some(client), Some(page), Some((x, y))) = (
            self.client.clone(),
            self.page.clone(),
            self.browser_coordinates(position),
        ) else {
            return;
        };
        let _ = gpui_tokio::Tokio::handle(cx).spawn(async move {
            let _ = client
                .dispatch_mouse_wheel(&page, x, y, delta_x, delta_y)
                .await;
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
                self.status = error.to_string().into();
                cx.notify();
                return;
            }
        };
        let endpoint = default_endpoint();
        let existing_client = self.client.clone();
        let existing_page = self.page.clone();
        let should_start_screencast = self.screencast_task.is_none();
        self.status = if existing_page.is_some() {
            "Navegando...".into()
        } else {
            format!("Conectando ao Obscura em {endpoint}...").into()
        };
        self.image = None;
        let task = gpui_tokio::Tokio::spawn_result(cx, async move {
            let client = match existing_client {
                Some(client) => client,
                None => match CdpClient::connect(&endpoint).await {
                    Ok(client) => client,
                    Err(connect_error) => CdpClient::launch_and_connect()
                        .await
                        .with_context(|| format!("{connect_error:#}"))?,
                },
            };
            let page = match existing_page {
                Some(page) => {
                    let mut page = page;
                    client.navigate(&mut page, &url).await?;
                    page
                }
                None => client.create_page(&url).await?,
            };
            let screenshot = client.capture_screenshot(&page).await?;
            let screencast = if should_start_screencast {
                client.start_screencast(&page).await.ok()
            } else {
                None
            };
            Ok(BrowserNavigation {
                client,
                page,
                screenshot,
                screencast,
            })
        });
        self.navigation_task = Some(cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(navigation) => {
                this.update_in(cx, |this, window, cx| {
                    let client = navigation.client.clone();
                    let page = navigation.page.clone();
                    this.client = Some(client.clone());
                    this.page = Some(page.clone());
                    this.image = Some(Arc::new(Image::from_bytes(
                        ImageFormat::Png,
                        navigation.screenshot,
                    )));
                    this.set_address_bar(&page.url, window, cx);
                    this.status = page.url.clone().into();
                    if let Some(events) = navigation.screencast {
                        this.start_screencast(client, page, events, window, cx);
                    }
                    cx.notify();
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.status = format!("Falha no navegador: {error:#}").into();
                    cx.notify();
                })
                .ok();
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
        self.screencast_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if event.method != "Page.screencastFrame"
                    || event.session_id.as_deref() != Some(&page.session_id)
                {
                    continue;
                }
                let Some(encoded) = event.params.get("data").and_then(Value::as_str) else {
                    continue;
                };
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
                    continue;
                };
                if let Some(session_id) = event.params.get("sessionId").and_then(Value::as_i64) {
                    let _ = client.acknowledge_screencast_frame(&page, session_id).await;
                }
                this.update(cx, |this, cx| {
                    this.image = Some(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)));
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn navigate_history(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(mut page)) = (self.client.clone(), self.page.clone()) else {
            return;
        };
        self.status = if delta < 0 {
            "Voltando...".into()
        } else {
            "Avançando...".into()
        };
        self.image = None;
        self.navigation_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                client.navigate_history(&mut page, delta).await?;
                let screenshot = client.capture_screenshot(&page).await?;
                anyhow::Ok((page, screenshot))
            }
            .await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok((page, screenshot)) => {
                        this.page = Some(page.clone());
                        this.image =
                            Some(Arc::new(Image::from_bytes(ImageFormat::Png, screenshot)));
                        this.set_address_bar(&page.url, window, cx);
                        this.status = page.url.into();
                    }
                    Err(error) => this.status = format!("Falha no histórico: {error:#}").into(),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(mut page)) = (self.client.clone(), self.page.clone()) else {
            self.navigate(window, cx);
            return;
        };
        self.status = "Atualizando...".into();
        self.image = None;
        self.navigation_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let url = page.url.clone();
                client.navigate(&mut page, &url).await?;
                let screenshot = client.capture_screenshot(&page).await?;
                anyhow::Ok((page, screenshot))
            }
            .await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok((page, screenshot)) => {
                        this.page = Some(page.clone());
                        this.image =
                            Some(Arc::new(Image::from_bytes(ImageFormat::Png, screenshot)));
                        this.set_address_bar(&page.url, window, cx);
                        this.status = page.url.into();
                    }
                    Err(error) => this.status = format!("Falha ao atualizar: {error:#}").into(),
                }
                cx.notify();
            })
            .ok();
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
        let address_bar = self.address_bar.clone().expect("browser address bar");
        let status = self.status.clone();
        let image = self.image.clone();
        let content_bounds_entity = cx.weak_entity();
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
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
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
                    .child(div().flex_1().min_w_0().child(address_bar))
                    .child(
                        IconButton::new("browser-go", IconName::ArrowRight)
                            .tooltip(Tooltip::text("Navegar"))
                            .on_click(cx.listener(|this, _, window, cx| this.navigate(window, cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .on_children_prepainted(move |children, _window, cx| {
                        if let Some(bounds) = children.first().copied() {
                            content_bounds_entity
                                .update(cx, |this, _cx| this.content_bounds = Some(bounds))
                                .ok();
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _window, cx| {
                            this.dispatch_mouse(
                                "mousePressed",
                                event.position,
                                event.click_count,
                                cx,
                            );
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseUpEvent, _window, cx| {
                            this.dispatch_mouse(
                                "mouseReleased",
                                event.position,
                                event.click_count,
                                cx,
                            );
                        }),
                    )
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
                    .when_some(image.clone(), |this, image| {
                        this.child(img(image).size_full())
                    })
                    .when(image.is_none(), |this| {
                        this.child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .child(Icon::new(IconName::Public).size(IconSize::XLarge))
                                .child(Label::new(status).color(Color::Muted)),
                        )
                    }),
            )
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
