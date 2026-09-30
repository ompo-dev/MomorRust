use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "getVersion" => Ok(json!({
            "protocolVersion": "1.3",
            "product": "Chrome/145.0.0.0",
            "revision": "@0000000000000000000000000000000000000000",
            "userAgent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
            "jsVersion": "14.5.0.0",
        })),
        "close" => {
            Ok(json!({}))
        }
        "getWindowForTarget" => Ok(json!({
            "windowId": 1,
            "bounds": {
                "left": 0,
                "top": 0,
                "width": 1280,
                "height": 720,
                "windowState": "normal",
            }
        })),
        "setDownloadBehavior" => set_download_behavior(params, ctx, session_id),
        "getWindowBounds" => Ok(json!({
            "bounds": { "left": 0, "top": 0, "width": 1280, "height": 720, "windowState": "normal" }
        })),
        // No-op acks for window-management methods Playwright sends during
        // page setup. We don't model real OS windows, but answering with {}
        // lets the client's setup sequence complete instead of tearing down
        // the page on an unknown-method error.
        "setWindowBounds" => Ok(json!({})),
        // Playwright grants permissions (geolocation, notifications, ...) per
        // browser context during setup. obscura does not gate any API on a
        // permission grant today, so the honest answer is to accept and
        // remember nothing; an unknown-method error would abort the client's
        // whole context initialization.
        "grantPermissions" | "resetPermissions" => Ok(json!({})),
        _ => Err(format!("Unknown Browser method: {}", method)),
    }
}

fn set_download_behavior(
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    let behavior = params
        .get("behavior")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let path = match behavior {
        "allow" => Some(
            params
                .get("downloadPath")
                .and_then(Value::as_str)
                .ok_or("Browser.setDownloadBehavior requires downloadPath when behavior is allow")?
                .into(),
        ),
        "deny" | "default" => None,
        other => return Err(format!("unsupported download behavior: {other}")),
    };
    if let Some(page) = ctx.get_session_page(session_id) {
        page.context.set_download_dir(path)?;
    } else {
        ctx.default_context.set_download_dir(path)?;
    }
    Ok(json!({}))
}
