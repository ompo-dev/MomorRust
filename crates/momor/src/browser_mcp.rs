//! Minimal stdio MCP bridge for Momor's visible WebView2 browser.
//!
//! The bridge deliberately talks to the same DevTools target as the visible
//! browser instead of starting another browser instance. This lets ACP agents
//! share the page without knowing which model started the session.

use anyhow::{Context as _, Result, anyhow};
use base64::Engine as _;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout};

use crate::browser::CdpClient;

const DEFAULT_CDP_PORT: u16 = 9224;
const INTERACTIVE_SELECTOR: &str = "a,button,input,textarea,select,[role=button],[role=link],[role=textbox],[contenteditable=true]";

pub fn init(cx: &mut gpui::App) {
    let registry =
        project::context_server_store::registry::ContextServerDescriptorRegistry::default_global(
            cx,
        );
    registry.update(cx, |registry, cx| {
        registry.register_context_server_descriptor(
            "momor-browser".into(),
            std::sync::Arc::new(BrowserServerDescriptor),
            cx,
        );
    });
}

struct BrowserServerDescriptor;

impl project::context_server_store::registry::ContextServerDescriptor for BrowserServerDescriptor {
    fn command(
        &self,
        _worktrees: gpui::Entity<project::worktree_store::WorktreeStore>,
        _cx: &gpui::AsyncApp,
    ) -> gpui::Task<Result<context_server::ContextServerCommand>> {
        gpui::Task::ready(
            std::env::current_exe()
                .context("failed to locate Momor browser bridge")
                .map(|path| context_server::ContextServerCommand {
                    path,
                    args: vec!["--browser-mcp".into()],
                    env: Some(collections::HashMap::from_iter([(
                        "MOMOR_BROWSER_CDP_PORT".into(),
                        browser_port().to_string(),
                    )])),
                    timeout: None,
                }),
        )
    }

    fn configuration(
        &self,
        _worktrees: gpui::Entity<project::worktree_store::WorktreeStore>,
        _cx: &gpui::AsyncApp,
    ) -> gpui::Task<Result<Option<extension::ContextServerConfiguration>>> {
        gpui::Task::ready(Ok(None))
    }
}

fn browser_port() -> u16 {
    std::env::var("MOMOR_BROWSER_CDP_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|port: &u16| *port != 0)
        .unwrap_or(DEFAULT_CDP_PORT)
}

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create browser MCP runtime")?;
    runtime.block_on(run_server())
}

async fn run_server() -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();
    let mut client = None;
    let mut inspection_only = true;

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": {"code": -32700, "message": error.to_string()},
                    }),
                )
                .await?;
                continue;
            }
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let Some(id) = request.get("id").cloned() else {
            continue;
        };

        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "momor-browser", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "These tools control Momor's live WebView2 tabs, not BrowserOS or Codex's in-app browser. Start with browser_tabs, match the attached tab, and pass target_id. Inspect DOM/accessibility and browser_frames before acting. Use frame_id to inspect embedded documents. Sessions start in inspection_only mode: use browser_set_mode actions only for user-requested actions, then inspect after each step and refresh refs. Never navigate, reload, or close an existing user activity to diagnose it. browser_navigate requires allow_navigation=true only for an explicitly requested navigation. Screenshots are only for genuinely visual tasks. Do not load browseros-neo or use cua_repl; neither exposes Momor. Report actual errors.",
            }),
            "tools/list" => json!({"tools": tool_definitions()}),
            "tools/call" => {
                match call_tool(&mut client, &mut inspection_only, request.get("params")).await {
                    Ok(result) => result,
                    Err(error) => json!({
                        "content": [{"type": "text", "text": error.to_string()}],
                        "isError": true,
                    }),
                }
            }
            _ => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": format!("method not found: {method}")},
                    }),
                )
                .await?;
                continue;
            }
        };
        write_response(
            &mut stdout,
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
        )
        .await?;
    }
    Ok(())
}

async fn call_tool(
    client: &mut Option<(String, CdpClient)>,
    inspection_only: &mut bool,
    params: Option<&Value>,
) -> Result<Value> {
    let params = params.context("tools/call requires params")?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .context("tools/call requires a tool name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if name == "browser_set_mode" {
        let mode = required_string(&arguments, "mode")?;
        *inspection_only = match mode.as_str() {
            "inspection_only" => true,
            "actions" => false,
            _ => anyhow::bail!("mode must be inspection_only or actions"),
        };
        return Ok(text_result(format!("Browser mode: {mode}")));
    }
    anyhow::ensure!(
        !*inspection_only || read_only_tool(name),
        "Inspection-only mode blocks this action. Inspect first; use browser_set_mode actions only when the user requested an action."
    );
    let targets = timeout(
        Duration::from_secs(5),
        discover_page_targets(browser_port()),
    )
    .await
    .context("timed out discovering Momor browser tabs")??;
    if name == "browser_tabs" {
        let mut tabs = Vec::new();
        for target in &targets {
            let browser = connect_to_target(client, target).await?;
            let page = browser.active_page().await?;
            let visible = browser
                .evaluate(&page, "document.visibilityState === 'visible'")
                .await?;
            tabs.push(json!({"target_id": target.id, "url": target.url, "title": target.title, "visible": visible, "ownership": "user", "preserve_state": true, "inspection_only": *inspection_only}));
        }
        return Ok(text_result(serde_json::to_string_pretty(&tabs)?));
    }
    let browser = if let Some(target_id) = arguments.get("target_id").and_then(Value::as_str) {
        let target = targets
            .iter()
            .find(|target| target.id == target_id)
            .context("Momor tab is no longer open; call browser_tabs again")?;
        connect_to_target(client, target).await?
    } else {
        let mut visible_browser = None;
        for target in &targets {
            let browser = connect_to_target(client, target).await?;
            let page = browser.active_page().await?;
            if browser
                .evaluate(&page, "document.visibilityState === 'visible'")
                .await?
                == Value::Bool(true)
                || targets.len() == 1
            {
                visible_browser = Some(browser);
                break;
            }
        }
        visible_browser.context("No visible Momor tab; select a target_id from browser_tabs")?
    };
    match execute_tool(&browser, name, &arguments).await {
        Ok(result) => Ok(result),
        Err(error) => {
            *client = None;
            Err(error)
        }
    }
}

async fn execute_tool(client: &CdpClient, name: &str, arguments: &Value) -> Result<Value> {
    let page = client.active_page().await?;
    let frame_id = arguments.get("frame_id").and_then(Value::as_str);
    match name {
        "browser_frames" => {
            let tree = client.send("Page.getFrameTree", json!({})).await?;
            let mut frames = Vec::new();
            if let Some(tree) = tree.get("frameTree") {
                collect_frames(tree, &mut frames);
            }
            Ok(text_result(serde_json::to_string_pretty(&frames)?))
        }
        "browser_state" => {
            let state = client.evaluate_in_frame(&page, frame_id,
                "({url:location.href,title:document.title,ready_state:document.readyState,visibility:document.visibilityState,time_origin:performance.timeOrigin,focused:{tag:document.activeElement?.tagName,name:document.activeElement?.getAttribute('aria-label') || document.activeElement?.name || ''},forms:Array.from(document.querySelectorAll('input,textarea,select,button')).slice(0,300).map(element=>({tag:element.tagName,name:element.getAttribute('aria-label') || element.name || element.innerText || '',type:element.type,value:element.type==='password' || element.type==='hidden' ? null : element.value,checked:element.checked,disabled:element.disabled})),media:Array.from(document.querySelectorAll('video,audio')).map(element=>({paused:element.paused,time:element.currentTime,ended:element.ended}))})"
            ).await?;
            Ok(text_result(serde_json::to_string_pretty(&state)?))
        }
        "browser_accessibility" => {
            let selector = serde_json::to_string(INTERACTIVE_SELECTOR)?;
            let accessibility = client
                .evaluate_in_frame(&page, frame_id, &format!("(() => {{ const epoch = globalThis.crypto?.randomUUID?.() || Date.now().toString(36) + '-' + Math.random().toString(36).slice(2); const refs = new Map(); window.__momorBrowserRefs = {{url: location.href, refs}}; const name = element => (element.getAttribute('aria-label') || element.getAttribute('aria-labelledby') || element.innerText || (element.type === 'password' ? '' : element.value) || element.getAttribute('title') || element.querySelector('img')?.alt || '').trim().replace(/\\s+/g, ' ').slice(0, 240); const role = element => element.getAttribute('role') || element.tagName.toLowerCase(); const interactive = Array.from(document.querySelectorAll({selector})).slice(0, 300).map((element, index) => {{ const ref = 'e' + (index + 1) + '-' + epoch; refs.set(ref, element); return {{ref, role: role(element), name: name(element), value: element.type === 'password' ? null : element.value || '', href: element.href || null, disabled: !!element.disabled}}; }}); return {{url: location.href, title: document.title, text: (document.body?.innerText || '').slice(0, 30000), headings: Array.from(document.querySelectorAll('h1,h2,h3')).slice(0, 100).map(element => ({{level: element.tagName.toLowerCase(), name: name(element)}})), landmarks: Array.from(document.querySelectorAll('main,nav,header,footer,aside,[role=main],[role=navigation],[role=dialog]')).slice(0, 50).map(element => ({{role: role(element), name: name(element)}})), iframes: Array.from(document.querySelectorAll('iframe')).map(element => ({{url: element.src, title: element.title, name: element.name, cross_origin: (() => {{try {{return !element.contentDocument;}} catch {{return true;}}}})()}})), interactive}}; }})()"),
                )
                .await?;
            Ok(text_result(serde_json::to_string_pretty(&accessibility)?))
        }
        "browser_dom" => {
            let dom = client
                .evaluate_in_frame(&page, frame_id, "document.documentElement?.outerHTML || ''")
                .await?;
            Ok(text_result(dom.to_string()))
        }
        "browser_navigate" => {
            anyhow::ensure!(frame_id.is_none(), "Navigate targets a tab, not a frame");
            let current_url = client.evaluate(&page, "location.href").await?;
            anyhow::ensure!(
                current_url == "about:blank"
                    || arguments.get("allow_navigation").and_then(Value::as_bool) == Some(true),
                "This is an existing user activity. Do not navigate for inspection. Set allow_navigation=true only for an explicit user navigation request."
            );
            let url = required_string(arguments, "url")?;
            let mut page = page;
            client.navigate(&mut page, &url).await?;
            Ok(text_result(format!("Navigated to {url}")))
        }
        "browser_click" => {
            let element = referenced_element(arguments)?;
            let expression = format!(
                "(() => {{ {element} element.scrollIntoView({{block:'center'}}); window.__momorBrowserRefs.refs.clear(); element.click(); return true; }})()"
            );
            client
                .evaluate_in_frame(&page, frame_id, &expression)
                .await?;
            Ok(text_result("Clicked element"))
        }
        "browser_fill" => {
            let element = referenced_element(arguments)?;
            let value = required_string(arguments, "value")?;
            let value = serde_json::to_string(&value)?;
            let expression = format!(
                "(() => {{ {element} if (element.disabled || element.readOnly) throw new Error('element is not editable'); element.focus(); window.__momorBrowserRefs.refs.clear(); const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), 'value')?.set; if (setter) setter.call(element, {value}); else if (element.isContentEditable) element.textContent = {value}; else throw new Error('element is not an input'); element.dispatchEvent(new Event('input', {{bubbles:true}})); element.dispatchEvent(new Event('change', {{bubbles:true}})); return element.value ?? element.textContent; }})()"
            );
            let result = client
                .evaluate_in_frame(&page, frame_id, &expression)
                .await?;
            Ok(text_result(result.to_string()))
        }
        "browser_type" => {
            let text = required_string(arguments, "text")?;
            client
                .evaluate_in_frame(&page, frame_id, &format!(
                        "document.activeElement && document.activeElement.dispatchEvent(new InputEvent('beforeinput', {{bubbles:true, data:{}, inputType:'insertText'}}))",
                        serde_json::to_string(&text)?
                    ),
                )
                .await?;
            client.insert_text(&page, &text).await?;
            Ok(text_result("Typed text"))
        }
        "browser_press" => {
            let key = required_string(arguments, "key")?;
            client
                .dispatch_key_event(&page, "keyDown", &key, "", 0)
                .await?;
            client
                .dispatch_key_event(&page, "keyUp", &key, "", 0)
                .await?;
            Ok(text_result(format!("Pressed {key}")))
        }
        "browser_focus" => {
            let element = referenced_element(arguments)?;
            let focused = client.evaluate_in_frame(&page, frame_id, &format!(
                "(() => {{ {element} element.scrollIntoView({{block:'center'}}); element.focus(); return document.activeElement === element; }})()"
            )).await?;
            anyhow::ensure!(
                focused == Value::Bool(true),
                "the element did not accept focus"
            );
            Ok(text_result("Focused element"))
        }
        "browser_click_at" => {
            let x = required_number(arguments, "x")?;
            let y = required_number(arguments, "y")?;
            anyhow::ensure!(
                x >= 0.0 && y >= 0.0,
                "coordinates must be nonnegative viewport pixels"
            );
            for event in ["mousePressed", "mouseReleased"] {
                client
                    .send(
                        "Input.dispatchMouseEvent",
                        json!({"type":event,"x":x,"y":y,"button":"left","clickCount":1}),
                    )
                    .await?;
            }
            Ok(text_result(
                "Clicked viewport coordinates; inspect state before the next action",
            ))
        }
        "browser_scroll" => {
            let amount = arguments
                .get("amount")
                .and_then(Value::as_f64)
                .unwrap_or(600.0);
            client
                .evaluate_in_frame(&page, frame_id, &format!("window.scrollBy(0, {amount})"))
                .await?;
            Ok(text_result(format!("Scrolled by {amount}")))
        }
        "browser_read" => {
            let text = client
                .evaluate_in_frame(&page, frame_id, "document.body?.innerText || ''")
                .await?;
            Ok(text_result(text.to_string()))
        }
        "browser_evaluate" => {
            let expression = required_string(arguments, "expression")?;
            let result = client
                .evaluate_in_frame(&page, frame_id, &expression)
                .await?;
            Ok(text_result(result.to_string()))
        }
        "browser_screenshot" => {
            let bytes = client.capture_screenshot(&page).await?;
            Ok(json!({
                "content": [{
                    "type": "image",
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                    "mimeType": "image/png",
                }]
            }))
        }
        _ => Err(anyhow!("unknown Momor browser tool: {name}")),
    }
}

fn required_string(arguments: &Value, field: &str) -> Result<String> {
    arguments
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("missing string argument: {field}"))
}

fn referenced_element(arguments: &Value) -> Result<String> {
    let reference = required_string(arguments, "ref")?;
    let reference = serde_json::to_string(&reference)?;
    Ok(format!(
        "const snapshot = window.__momorBrowserRefs; const element = snapshot?.refs.get({reference}); if (!element?.isConnected || snapshot.url !== location.href) throw new Error('stale browser ref: call browser_accessibility again in the same tab/frame');"
    ))
}

fn required_number(arguments: &Value, field: &str) -> Result<f64> {
    arguments
        .get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .with_context(|| format!("missing finite number argument: {field}"))
}

fn collect_frames(tree: &Value, frames: &mut Vec<Value>) {
    if let Some(frame) = tree.get("frame") {
        frames.push(json!({"frame_id":frame["id"],"parent_id":frame["parentId"],"url":frame["url"],"name":frame["name"],"security_origin":frame["securityOrigin"]}));
    }
    if let Some(children) = tree.get("childFrames").and_then(Value::as_array) {
        for child in children {
            collect_frames(child, frames);
        }
    }
}

fn read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "browser_tabs"
            | "browser_frames"
            | "browser_state"
            | "browser_accessibility"
            | "browser_dom"
            | "browser_read"
            | "browser_screenshot"
    )
}

fn text_result(text: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}]})
}

async fn connect_to_target(
    client: &mut Option<(String, CdpClient)>,
    target: &BrowserTarget,
) -> Result<CdpClient> {
    if let Some((endpoint, browser)) = client.as_ref()
        && endpoint == &target.endpoint
    {
        return Ok(browser.clone());
    }
    let browser = timeout(
        Duration::from_secs(5),
        CdpClient::connect_page(&target.endpoint),
    )
    .await
    .context("timed out connecting to the Momor browser tab")??;
    *client = Some((target.endpoint.clone(), browser.clone()));
    Ok(browser)
}

#[derive(serde::Deserialize)]
struct BrowserTarget {
    id: String,
    url: String,
    title: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    endpoint: String,
}

async fn discover_page_targets(port: u16) -> Result<Vec<BrowserTarget>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("Momor browser is not listening on CDP port {port}"))?;
    stream
        .write_all(
            format!(
                "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let mut response = Vec::new();
    let mut buffer = [0_u8; 4096];
    let body_start = loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break response
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4)
                .context("invalid browser CDP response")?;
        }
        response.extend_from_slice(&buffer[..read]);
        let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let content_length =
            std::str::from_utf8(&response[..header_end])
                .ok()
                .and_then(|headers| {
                    headers.lines().find_map(|line| {
                        line.strip_prefix("Content-Length:")
                            .or_else(|| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                });
        if content_length.is_some_and(|length| response.len() >= header_end + 4 + length) {
            break header_end + 4;
        }
    };
    let body = response
        .get(body_start..)
        .context("invalid browser CDP response body")?;
    let targets: Vec<Value> =
        serde_json::from_slice(body).context("invalid browser CDP target list")?;
    let targets = targets
        .into_iter()
        .filter(|target| target.get("type").and_then(Value::as_str) == Some("page"))
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<BrowserTarget>, _>>()
        .context("invalid Momor browser page target")?;
    anyhow::ensure!(!targets.is_empty(), "Momor browser has no open tabs");
    Ok(targets)
}

async fn write_response(stdout: &mut tokio::io::Stdout, response: Value) -> Result<()> {
    stdout.write_all(response.to_string().as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "browser_set_mode",
            "Start in inspection_only. Enable actions only for user-requested actions after inspecting the actual tab/frame.",
            json!({"type":"object","required":["mode"],"properties":{"mode":{"type":"string","enum":["inspection_only","actions"]}}}),
        ),
        tool(
            "browser_frames",
            "List frame_id, parent, origin and URL for the entire frame tree, including cross-origin iframes. Inspect before acting.",
            json!({"type":"object"}),
        ),
        tool(
            "browser_state",
            "Read URL, title, load/focus state, form values, enabled buttons and media state without navigating or taking a screenshot. Passwords/hidden fields are redacted.",
            json!({"type":"object"}),
        ),
        tool(
            "browser_focus",
            "Focus an element ref in the selected frame for subsequent browser_type/browser_press.",
            json!({"type":"object","required":["ref"],"properties":{"ref":{"type":"string"}}}),
        ),
        tool(
            "browser_click_at",
            "Click viewport coordinates (CSS pixels), including inside inaccessible iframes. Inspect to establish coordinates, then verify state; never navigate as a fallback.",
            json!({"type":"object","required":["x","y"],"properties":{"x":{"type":"number"},"y":{"type":"number"}}}),
        ),
        tool(
            "browser_tabs",
            "List Momor's open tabs with target_id, URL, title, and visibility. Match an attached tab before acting.",
            json!({"type":"object"}),
        ),
        tool(
            "browser_accessibility",
            "Read the page's text, headings, landmarks, and interactive refs from DOM/accessibility data. This never captures an image.",
            json!({"type":"object"}),
        ),
        tool(
            "browser_dom",
            "Read the current page HTML. Use this for normal inspection instead of a screenshot.",
            json!({"type":"object"}),
        ),
        tool(
            "browser_navigate",
            "Navigate a tab only on explicit user request. Never navigate an existing activity for inspection; allow_navigation=true confirms the user's navigation instruction.",
            json!({"type":"object","required":["url"],"properties":{"url":{"type":"string"},"allow_navigation":{"type":"boolean","default":false}}}),
        ),
        tool(
            "browser_click",
            "Click an element ref from the latest browser_accessibility result.",
            json!({"type":"object","required":["ref"],"properties":{"ref":{"type":"string"}}}),
        ),
        tool(
            "browser_fill",
            "Replace an input value and dispatch input/change events.",
            json!({"type":"object","required":["ref","value"],"properties":{"ref":{"type":"string"},"value":{"type":"string"}}}),
        ),
        tool(
            "browser_type",
            "Type into the currently focused page element.",
            json!({"type":"object","required":["text"],"properties":{"text":{"type":"string"}}}),
        ),
        tool(
            "browser_press",
            "Press a page keyboard key such as Enter or Escape.",
            json!({"type":"object","required":["key"],"properties":{"key":{"type":"string"}}}),
        ),
        tool(
            "browser_scroll",
            "Scroll the visible page vertically.",
            json!({"type":"object","properties":{"amount":{"type":"number"}}}),
        ),
        tool(
            "browser_read",
            "Extract the visible page text.",
            json!({"type":"object"}),
        ),
        tool(
            "browser_evaluate",
            "Evaluate JavaScript in the visible page.",
            json!({"type":"object","required":["expression"],"properties":{"expression":{"type":"string"}}}),
        ),
        tool(
            "browser_screenshot",
            "Capture the visible page as a PNG only when visual inspection is genuinely necessary; do not use for normal navigation or reading.",
            json!({"type":"object"}),
        ),
    ]
}

fn tool(name: &str, description: &str, mut input_schema: Value) -> Value {
    input_schema["properties"]["target_id"] = json!({"type":"string", "description":"Momor tab target_id from browser_tabs. Omit to use the currently visible tab."});
    if matches!(
        name,
        "browser_accessibility"
            | "browser_dom"
            | "browser_state"
            | "browser_click"
            | "browser_fill"
            | "browser_focus"
            | "browser_type"
            | "browser_read"
            | "browser_evaluate"
            | "browser_scroll"
    ) {
        input_schema["properties"]["frame_id"] = json!({"type":"string","description":"frame_id from browser_frames. Omit for the top document. Use the same frame for inspection and refs."});
    }
    json!({"name": name, "description": description, "inputSchema": input_schema, "annotations":{"readOnlyHint":read_only_tool(name),"destructiveHint":matches!(name,"browser_navigate"|"browser_evaluate")}})
}
