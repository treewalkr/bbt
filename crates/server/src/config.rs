//! Settings from the environment (`.env`) and the one-call live sync used by the
//! CLI and the HTTP API.

use crate::store::Store;
use crate::sync::{self, Outcome, Tracker};
use crate::tracker::{self, Client, Credentials, Site};
use anyhow::{Context, Result};
use std::time::Duration;

/// Minimum gap between any two requests to the tracker.
pub const REQUEST_GAP: Duration = Duration::from_secs(3);

pub struct Config {
    pub db_path: String,
    pub base_url: String,
    pub creds: Credentials,
    pub site: Site,
    /// `TRACKER_DEVICE_ID`, if set. Otherwise the stored or a newly generated id is used.
    pub device_id: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            db_path: std::env::var("BBT_DB").unwrap_or_else(|_| "bbt.db".into()),
            base_url: required("TRACKER_BASE_URL")?,
            creds: Credentials {
                username: required("TRACKER_USERNAME")?,
                password: required("TRACKER_PASSWORD")?,
            },
            site: site_from_env(),
            device_id: std::env::var("TRACKER_DEVICE_ID").ok(),
        })
    }
}

/// `TRACKER_LIST_PATH`, `TRACKER_DEVICE_COOKIE`, `TRACKER_HASH_PARAM` and
/// `TRACKER_CATEGORIES`, each optional.
fn site_from_env() -> Site {
    let d = Site::default();
    let var = |key: &str, default: String| {
        std::env::var(key)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or(default)
    };
    Site {
        list_path: var("TRACKER_LIST_PATH", d.list_path),
        device_cookie: var("TRACKER_DEVICE_COOKIE", d.device_cookie),
        hash_param: var("TRACKER_HASH_PARAM", d.hash_param),
        categories: parse_categories(&var("TRACKER_CATEGORIES", String::new())),
    }
}

/// `"1:Movies,2:Series"` → `[(1, "Movies"), (2, "Series")]`. Bad entries are skipped.
fn parse_categories(s: &str) -> Vec<(u32, String)> {
    s.split(',')
        .filter_map(|entry| {
            let (id, name) = entry.split_once(':')?;
            let name = name.trim();
            Some((id.trim().parse().ok()?, name.to_string())).filter(|_| !name.is_empty())
        })
        .collect()
}

/// A finished live sync and how many HTTP requests it made.
pub struct LiveSync {
    pub outcome: Outcome,
    pub requests: u32,
}

/// A client using the stored device id and session cookies.
pub fn client(store: &Store, cfg: &Config) -> Result<Client> {
    let device_id = device_id(store, cfg.device_id.as_deref())?;
    let cookies = store.get("cookies")?;
    Ok(Client::new(
        &cfg.base_url,
        &cfg.site.device_cookie,
        &device_id,
        cookies.as_deref(),
        REQUEST_GAP,
    )?)
}

/// Sync against the real site. Saves cookies even when the sync fails.
pub async fn sync_now(
    store: &mut Store,
    cfg: &Config,
    client: &mut Client,
    max_pages: u32,
) -> Result<LiveSync> {
    let before = client.requests();
    let now = chrono::Utc::now().timestamp();
    let mut source = Tracker {
        client: &mut *client,
        creds: &cfg.creds,
        site: &cfg.site,
    };
    let outcome = sync::run(store, &mut source, max_pages, now).await;
    store.set("cookies", &client.cookie_header())?;
    Ok(LiveSync {
        outcome: outcome?,
        requests: client.requests() - before,
    })
}

/// Reuse one device id forever: the configured one, else the stored one, else a new one.
fn device_id(store: &Store, configured: Option<&str>) -> Result<String> {
    if let Some(id) = configured {
        store.set("device_id", id)?;
        return Ok(id.to_string());
    }
    if let Some(id) = store.get("device_id")? {
        return Ok(id);
    }
    let id = tracker::generate_device_id();
    store.set("device_id", &id)?;
    Ok(id)
}

fn required(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} is missing from .env"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_categories() {
        assert_eq!(
            parse_categories("1:Movies, 2 : TV Series,bad,3:,x:Y"),
            vec![(1, "Movies".to_string()), (2, "TV Series".to_string())]
        );
        assert!(parse_categories("").is_empty());
    }

    #[test]
    fn device_id_is_stable() {
        let s = Store::open_in_memory().unwrap();
        let first = device_id(&s, None).unwrap();
        assert_eq!(device_id(&s, None).unwrap(), first);
        assert_eq!(device_id(&s, Some("abc")).unwrap(), "abc");
        assert_eq!(device_id(&s, None).unwrap(), "abc");
    }
}
