use serde::{Deserialize, Serialize};

pub const SESSION_KEY: &str = "momor_browser_session";
const SESSION_VERSION: u32 = 1;
const MAX_HISTORY_ENTRIES: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedBrowserPage {
    pub url: String,
    pub title: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSession {
    pub version: u32,
    pub tabs: Vec<SavedBrowserPage>,
    pub active_tab: usize,
    #[serde(default)]
    pub history: Vec<SavedBrowserPage>,
}

impl Default for BrowserSession {
    fn default() -> Self {
        Self {
            version: SESSION_VERSION,
            tabs: vec![SavedBrowserPage {
                url: "about:blank".into(),
                title: "Nova aba".into(),
            }],
            active_tab: 0,
            history: Vec::new(),
        }
    }
}

impl BrowserSession {
    pub fn decode(json: &str) -> anyhow::Result<Self> {
        let mut session: Self = serde_json::from_str(json)?;
        anyhow::ensure!(
            session.version == SESSION_VERSION,
            "Unsupported browser session version"
        );
        for page in &mut session.tabs {
            if !restorable_url(&page.url) {
                *page = Self::default().tabs.remove(0);
            }
        }
        if session.tabs.is_empty() {
            session.tabs = Self::default().tabs;
        }
        session.active_tab = session.active_tab.min(session.tabs.len().saturating_sub(1));
        session
            .history
            .retain(|page| page.url != "about:blank" && restorable_url(&page.url));
        if session.history.len() > MAX_HISTORY_ENTRIES {
            session
                .history
                .drain(..session.history.len() - MAX_HISTORY_ENTRIES);
        }
        Ok(session)
    }

    pub fn record_visit(history: &mut Vec<SavedBrowserPage>, page: SavedBrowserPage) {
        if page.url == "about:blank" || !restorable_url(&page.url) {
            return;
        }
        if let Some(last) = history.last_mut().filter(|last| last.url == page.url) {
            *last = page;
        } else {
            history.push(page);
            if history.len() > MAX_HISTORY_ENTRIES {
                history.remove(0);
            }
        }
    }
}

fn restorable_url(value: &str) -> bool {
    value == "about:blank"
        || url::Url::parse(value)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_active_selection_and_history_survive_restart() -> anyhow::Result<()> {
        let mut session = BrowserSession::default();
        let page = SavedBrowserPage {
            url: "https://example.com/video".into(),
            title: "Video title".into(),
        };
        session.tabs.push(page.clone());
        session.active_tab = 1;
        BrowserSession::record_visit(&mut session.history, page);
        assert_eq!(
            BrowserSession::decode(&serde_json::to_string(&session)?)?,
            session
        );
        Ok(())
    }

    #[test]
    fn empty_tabs_invalid_urls_and_active_index_are_repaired() -> anyhow::Result<()> {
        let session = BrowserSession::decode(r#"{"version":1,"tabs":[],"active_tab":99}"#)?;
        assert_eq!(session, BrowserSession::default());
        let session = BrowserSession::decode(
            r#"{"version":1,"tabs":[{"url":"javascript:alert(1)","title":"bad"}],"active_tab":99}"#,
        )?;
        assert_eq!(session.tabs, BrowserSession::default().tabs);
        assert_eq!(session.active_tab, 0);
        assert!(BrowserSession::decode(r#"{"version":2,"tabs":[],"active_tab":0}"#).is_err());
        assert!(BrowserSession::decode("broken").is_err());
        Ok(())
    }

    #[test]
    fn visits_survive_closed_tabs_and_skip_blank_pages() {
        let mut history = Vec::new();
        BrowserSession::record_visit(&mut history, BrowserSession::default().tabs.remove(0));
        assert!(history.is_empty());
        let page = SavedBrowserPage {
            url: "https://example.com/".into(),
            title: "Example".into(),
        };
        BrowserSession::record_visit(&mut history, page.clone());
        BrowserSession::record_visit(&mut history, page);
        assert_eq!(history.len(), 1);
    }
}
