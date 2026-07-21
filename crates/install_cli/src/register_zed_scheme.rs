use client::MOMOR_URL_SCHEME;
use gpui::{AsyncApp, actions};

actions!(
    cli,
    [
        /// Registers the momor:// URL scheme handler.
        RegisterMomorScheme
    ]
);

pub async fn register_momor_scheme(cx: &AsyncApp) -> anyhow::Result<()> {
    cx.update(|cx| cx.register_url_scheme(MOMOR_URL_SCHEME)).await
}
