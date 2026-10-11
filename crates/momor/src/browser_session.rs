use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
    #[serde(default = "keep_pages_active_by_default")]
    pub keep_pages_active: bool,
}

fn keep_pages_active_by_default() -> bool {
    true
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
            keep_pages_active: true,
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

    pub fn saved_links(urls: &[String], history: &[SavedBrowserPage]) -> Vec<SavedBrowserPage> {
        urls.iter()
            .filter(|url| url.as_str() != "about:blank" && restorable_url(url))
            .map(|url| {
                history
                    .iter()
                    .rev()
                    .find(|page| &page.url == url)
                    .cloned()
                    .unwrap_or_else(|| SavedBrowserPage {
                        url: url.clone(),
                        title: url::Url::parse(url)
                            .ok()
                            .and_then(|url| url.host_str().map(str::to_owned))
                            .unwrap_or_else(|| url.clone()),
                    })
            })
            .collect()
    }

    pub fn suggestions(
        query: &str,
        favorites: &[String],
        history: &[SavedBrowserPage],
    ) -> Vec<SavedBrowserPage> {
        let query = query.trim().to_lowercase();
        let words: Vec<_> = query.split_whitespace().collect();
        let mut seen = HashSet::new();
        let mut candidates: Vec<_> = Self::saved_links(favorites, history)
            .into_iter()
            .chain(history.iter().rev().cloned())
            .enumerate()
            .filter_map(|(recency, page)| {
                if !restorable_url(&page.url)
                    || page.url == "about:blank"
                    || !seen.insert(page.url.clone())
                {
                    return None;
                }
                let url = page.url.to_lowercase();
                let text = format!("{url} {}", page.title.to_lowercase());
                if !words.iter().all(|word| text.contains(word)) {
                    return None;
                }
                let host_and_path = url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_start_matches("www.");
                let rank = if !query.is_empty() && host_and_path.starts_with(&query) {
                    0
                } else if favorites.contains(&page.url) {
                    1
                } else {
                    2
                };
                Some((rank, recency, page))
            })
            .collect();
        candidates.sort_by_key(|(rank, recency, _)| (*rank, *recency));
        candidates
            .into_iter()
            .take(8)
            .map(|(_, _, page)| page)
            .collect()
    }

    pub fn frequent_sites(history: &[SavedBrowserPage]) -> Vec<SavedBrowserPage> {
        let mut sites = HashMap::new();
        for (recency, page) in history.iter().enumerate() {
            let Ok(url) = url::Url::parse(&page.url) else {
                continue;
            };
            if !restorable_url(&page.url) || url.host_str().is_none() {
                continue;
            }
            let origin = format!("{}/", url.origin().ascii_serialization());
            let entry = sites.entry(origin.clone()).or_insert((
                0usize,
                recency,
                SavedBrowserPage {
                    url: origin,
                    title: url
                        .host_str()
                        .unwrap_or_default()
                        .trim_start_matches("www.")
                        .to_owned(),
                },
            ));
            entry.0 += 1;
            entry.1 = recency;
        }
        let mut sites: Vec<_> = sites.into_values().collect();
        sites.sort_by_key(|(visits, recency, _)| {
            (std::cmp::Reverse(*visits), std::cmp::Reverse(*recency))
        });
        sites
            .into_iter()
            .take(12)
            .map(|(_, _, page)| page)
            .collect()
    }
}

pub fn reorder_tabs<T>(tabs: &mut Vec<T>, active: &mut usize, from: usize, to: usize) -> bool {
    if from >= tabs.len() || to >= tabs.len() || from == to {
        return false;
    }
    let tab = tabs.remove(from);
    tabs.insert(to, tab);
    *active = if *active == from {
        to
    } else if from < *active && *active <= to {
        *active - 1
    } else if to <= *active && *active < from {
        *active + 1
    } else {
        *active
    };
    true
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

    #[test]
    fn suggestions_match_urls_titles_and_bookmarks_without_duplicates() {
        let history = vec![
            SavedBrowserPage {
                url: "https://github.com/example/repo".into(),
                title: "Rust project".into(),
            },
            SavedBrowserPage {
                url: "https://example.com/guide".into(),
                title: "Git guide".into(),
            },
            SavedBrowserPage {
                url: "https://github.com/example/repo".into(),
                title: "Updated Rust project".into(),
            },
        ];
        let favorites = vec!["https://github.com/".into()];
        let suggestions = BrowserSession::suggestions("git", &favorites, &history);
        assert_eq!(suggestions.len(), 3);
        assert_eq!(suggestions[0].url, "https://github.com/");
        assert_eq!(suggestions[1].title, "Updated Rust project");
        assert_eq!(
            BrowserSession::suggestions("rust project", &favorites, &history).len(),
            1
        );
        assert!(BrowserSession::suggestions("does not match", &favorites, &history).is_empty());
    }

    #[test]
    fn quick_links_rank_frequency_then_recency_and_keep_ports() {
        let history = vec![
            SavedBrowserPage {
                url: "https://example.com/old".into(),
                title: "old".into(),
            },
            SavedBrowserPage {
                url: "http://localhost:5173/test".into(),
                title: "test".into(),
            },
            SavedBrowserPage {
                url: "https://example.com/new".into(),
                title: "new".into(),
            },
            SavedBrowserPage {
                url: "https://other.example/new".into(),
                title: "other".into(),
            },
            SavedBrowserPage {
                url: "about:blank".into(),
                title: "blank".into(),
            },
        ];
        let sites = BrowserSession::frequent_sites(&history);
        assert_eq!(
            sites
                .iter()
                .map(|page| page.url.as_str())
                .collect::<Vec<_>>(),
            vec![
                "https://example.com/",
                "https://other.example/",
                "http://localhost:5173/"
            ]
        );
    }

    #[test]
    fn dragging_in_both_directions_preserves_the_active_tab() -> anyhow::Result<()> {
        for active in 0..4 {
            for from in 0..4 {
                for to in 0..4 {
                    let mut tabs = vec![0, 1, 2, 3];
                    let mut selected = active;
                    reorder_tabs(&mut tabs, &mut selected, from, to);
                    assert_eq!(tabs[selected], active);
                    assert_eq!(tabs[to], from);
                }
            }
        }
        let mut session = BrowserSession::default();
        session.tabs.push(SavedBrowserPage {
            url: "https://example.com/".into(),
            title: "Example".into(),
        });
        reorder_tabs(&mut session.tabs, &mut session.active_tab, 1, 0);
        let restored = BrowserSession::decode(&serde_json::to_string(&session)?)?;
        assert_eq!(restored.tabs, session.tabs);
        assert_eq!(restored.active_tab, 1);
        Ok(())
    }

    #[test]
    fn page_activity_preference_survives_restart_and_defaults_for_old_sessions()
    -> anyhow::Result<()> {
        let legacy = BrowserSession::decode(r#"{"version":1,"tabs":[],"active_tab":0}"#)?;
        assert!(legacy.keep_pages_active);
        let session = BrowserSession {
            keep_pages_active: false,
            ..BrowserSession::default()
        };
        assert!(!BrowserSession::decode(&serde_json::to_string(&session)?)?.keep_pages_active);
        Ok(())
    }
}
