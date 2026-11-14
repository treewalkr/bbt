//! Settings from the environment (`.env`) and the one-call live sync used by the
//! CLI and the HTTP API.

use crate::store::Store;
use crate::sync::{self, Outcome, Tracker};
use crate::tracker::{self, Client, Credentials};
use anyhow::{Context, Result};
use std::time::Duration;

/// Minimum gap between any two requests to tracker.
pub const REQUEST_GAP: Duration = Duration::from_secs(3);

pub struct Config {
    pub db_path: String,
    pub base_url: String,
    pub creds: Credentials,
    /// `TRACKER_DEVICE_ID`, if set. Otherwise the stored or a newly generated id is used.
    pub device_id: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            db_path: std::env::var("BBT_DB").unwrap_or_else(|_| "bbt.db".into()),
            base_url: std::env::var("TRACKER_BASE_URL")
                .unwrap_or_else(|_| tracker::BASE_URL.into()),
            creds: Credentials {
                username: required("TRACKER_USERNAME")?,
                password: required("TRACKER_PASSWORD")?,
            },
            device_id: std::env::var("TRACKER_DEVICE_ID").ok(),
        })
    }
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
    fn device_id_is_stable() {
        let s = Store::open_in_memory().unwrap();
        let first = device_id(&s, None).unwrap();
        assert_eq!(device_id(&s, None).unwrap(), first);
        assert_eq!(device_id(&s, Some("abc")).unwrap(), "abc");
        assert_eq!(device_id(&s, None).unwrap(), "abc");
    }
}
