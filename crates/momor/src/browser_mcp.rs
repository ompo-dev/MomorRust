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
                "instructions": "Momor browser tools control the visible browser page. Use browser_accessibility or browser_dom for normal navigation and refresh refs after navigation. These tools do not capture images. Use browser_screenshot only when visual inspection is genuinely necessary. Never use BrowserOS, BrowserOS Neo, browseros-neo, or any external browser; this MCP is the only browser for Momor.",
            }),
            "tools/list" => json!({"tools": tool_definitions()}),
            "tools/call" => match call_tool(&mut client, request.get("params")).await {
                Ok(result) => result,
                Err(error) => json!({
                    "content": [{"type": "text", "text": error.to_string()}],
                    "isError": true,
                }),
            },
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

async fn call_tool(client: &mut Option<CdpClient>, params: Option<&Value>) -> Result<Value> {
    let params = params.context("tools/call requires params")?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .context("tools/call requires a tool name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let browser = if let Some(existing) = client.as_ref() {
        existing.clone()
    } else {
        let connected = connect_to_visible_browser().await?;
        *client = Some(connected.clone());
        connected
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
    match name {
        "browser_accessibility" => {
            let accessibility = client
                .evaluate(
                    &page,
                    "(() => { const name = element => (element.getAttribute('aria-label') || element.getAttribute('aria-labelledby') || element.innerText || element.value || element.getAttribute('title') || '').trim().replace(/\\s+/g, ' ').slice(0, 240); const role = element => element.getAttribute('role') || element.tagName.toLowerCase(); const interactive = Array.from(document.querySelectorAll('a,button,input,textarea,select,[role=button],[role=link],[role=textbox],[contenteditable=true]')).slice(0, 300).map((element, index) => ({ref: 'e' + (index + 1), role: role(element), name: name(element), value: element.value || '', href: element.href || null, disabled: !!element.disabled})); return {url: location.href, title: document.title, text: (document.body?.innerText || '').slice(0, 30000), headings: Array.from(document.querySelectorAll('h1,h2,h3')).slice(0, 100).map(element => ({level: element.tagName.toLowerCase(), name: name(element)})), landmarks: Array.from(document.querySelectorAll('main,nav,header,footer,aside,[role=main],[role=navigation],[role=dialog]')).slice(0, 50).map(element => ({role: role(element), name: name(element)})), interactive}; })()",
                )
                .await?;
            Ok(text_result(serde_json::to_string_pretty(&accessibility)?))
        }
        "browser_dom" => {
            let dom = client
                .evaluate(
                    &page,
                    "document.documentElement?.outerHTML || ''",
                )
                .await?;
            Ok(text_result(dom.to_string()))
        }
        "browser_navigate" => {
            let url = required_string(arguments, "url")?;
            let mut page = page;
            client.navigate(&mut page, &url).await?;
            Ok(text_result(format!("Navigated to {url}")))
        }
        "browser_click" => {
            let index = ref_index(arguments)?;
            let expression = format!(
                "(() => {{ const elements = Array.from(document.querySelectorAll('a,button,input,textarea,select,[role=button]')); const element = elements[{index}]; if (!element) throw new Error('stale browser ref'); element.scrollIntoView({{block:'center'}}); element.click(); return true; }})()"
            );
            client.evaluate(&page, &expression).await?;
            Ok(text_result("Clicked element"))
        }
        "browser_fill" => {
            let index = ref_index(arguments)?;
            let value = required_string(arguments, "value")?;
            let value = serde_json::to_string(&value)?;
            let expression = format!(
                "(() => {{ const elements = Array.from(document.querySelectorAll('a,button,input,textarea,select,[role=button]')); const element = elements[{index}]; if (!element) throw new Error('stale browser ref'); element.focus(); const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), 'value')?.set; if (setter) setter.call(element, {value}); else element.value = {value}; element.dispatchEvent(new Event('input', {{bubbles:true}})); element.dispatchEvent(new Event('change', {{bubbles:true}})); return element.value; }})()"
            );
            let result = client.evaluate(&page, &expression).await?;
            Ok(text_result(result.to_string()))
        }
        "browser_type" => {
            let text = required_string(arguments, "text")?;
            client
                .evaluate(
                    &page,
                    &format!(
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
        "browser_scroll" => {
            let amount = arguments
                .get("amount")
                .and_then(Value::as_f64)
                .unwrap_or(600.0);
            client
                .evaluate(&page, &format!("window.scrollBy(0, {amount})"))
                .await?;
            Ok(text_result(format!("Scrolled by {amount}")))
        }
        "browser_read" => {
            let text = client
                .evaluate(&page, "document.body?.innerText || ''")
                .await?;
            Ok(text_result(text.to_string()))
        }
        "browser_evaluate" => {
            let expression = required_string(arguments, "expression")?;
            let result = client.evaluate(&page, &expression).await?;
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

fn ref_index(arguments: &Value) -> Result<usize> {
    let reference = required_string(arguments, "ref")?;
    let index = reference
        .strip_prefix('e')
        .context("browser ref must look like e12")?
        .parse::<usize>()
        .context("browser ref must contain a numeric index")?;
    index
        .checked_sub(1)
        .context("browser ref index must be greater than zero")
}

fn text_result(text: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}]})
}

async fn connect_to_visible_browser() -> Result<CdpClient> {
    let port = std::env::var("MOMOR_BROWSER_CDP_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|port: &u16| *port != 0)
        .unwrap_or(DEFAULT_CDP_PORT);
    let endpoint = timeout(Duration::from_secs(5), discover_page_endpoint(port))
        .await
        .context("timed out discovering the visible browser page")??;
    timeout(Duration::from_secs(5), CdpClient::connect_page(&endpoint))
        .await
        .context("timed out connecting to the visible browser page")?
}

async fn discover_page_endpoint(port: u16) -> Result<String> {
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
        let Some(header_end) = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        else {
            continue;
        };
        let content_length = std::str::from_utf8(&response[..header_end])
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
    let targets: Vec<Value> = serde_json::from_slice(body).context("invalid browser CDP target list")?;
    targets
        .into_iter()
        .find(|target| target.get("type").and_then(Value::as_str) == Some("page"))
        .and_then(|target| target.get("webSocketDebuggerUrl").and_then(Value::as_str).map(str::to_owned))
        .context("browser CDP target list has no visible page")
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
            "Navigate the visible browser page.",
            json!({"type":"object","required":["url"],"properties":{"url":{"type":"string"}}}),
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

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema})
}
