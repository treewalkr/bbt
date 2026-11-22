//! A local copy of every poster and screenshot the UI shows. Each image is read from
//! its host once and kept on disk; the browser is then told to keep it for good, so
//! going back to a torrent shows its pictures at once. Image hosts only: the tracker
//! is never asked (see `hosts::allowed`).

use crate::hosts;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use reqwest::{Client, Response, Url, redirect};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Bigger answers are not images worth keeping.
const MAX_IMAGE_BYTES: usize = 20 << 20;
const MAX_REDIRECTS: usize = 3;
/// When the copies pass the cap, the least recently shown are dropped down to this share.
const PRUNE_TO: f64 = 0.9;

pub struct ImageCache {
    dir: PathBuf,
    client: Client,
    /// The tracker's host name, never fetched from here.
    tracker: Option<String>,
    cap_bytes: u64,
    used_bytes: AtomicU64,
    pruning: AtomicBool,
    /// One fetch per image at a time: a second asker waits and then reads the copy.
    inflight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

pub struct Image {
    pub bytes: Vec<u8>,
    pub kind: &'static str,
}

impl ImageCache {
    pub fn open(
        dir: impl Into<PathBuf>,
        tracker: Option<String>,
        cap_bytes: u64,
    ) -> std::io::Result<Arc<Self>> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let used = files(&dir).iter().map(|f| f.1).sum();
        let client = Client::builder()
            .redirect(redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .user_agent("Mozilla/5.0 (compatible; bbt)")
            .build()
            .map_err(std::io::Error::other)?;
        Ok(Arc::new(Self {
            dir,
            client,
            tracker,
            cap_bytes,
            used_bytes: AtomicU64::new(used),
            pruning: AtomicBool::new(false),
            inflight: Mutex::default(),
        }))
    }

    /// The saved copy, or the image read from its host now and saved.
    /// `None` when the host refuses, fails, or answers with something that isn't an image.
    pub async fn get(self: &Arc<Self>, url: &str) -> Option<Image> {
        let key = key(url);
        if let Some(hit) = self.read(&key).await {
            return Some(hit);
        }
        let lock = self
            .inflight
            .lock()
            .expect("inflight lock")
            .entry(key.clone())
            .or_default()
            .clone();
        let guard = lock.lock().await;
        let got = match self.read(&key).await {
            Some(hit) => Some(hit), // another asker just saved it
            None => self.fetch_and_save(url, &key).await,
        };
        drop(guard);
        self.inflight.lock().expect("inflight lock").remove(&key);
        got
    }

    /// Save these images in the background, so they are ready before they are shown.
    pub fn warm(self: &Arc<Self>, urls: Vec<String>) {
        let me = self.clone();
        tokio::spawn(async move {
            for url in urls {
                me.get(&url).await;
            }
        });
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    async fn read(&self, key: &str) -> Option<Image> {
        let path = self.path(key);
        let bytes = tokio::fs::read(&path).await.ok()?;
        let kind = sniff(&bytes)?;
        // Mark it recently shown, so pruning keeps it.
        let _ = tokio::task::spawn_blocking(move || {
            std::fs::File::options()
                .write(true)
                .open(path)
                .and_then(|f| f.set_modified(SystemTime::now()))
        })
        .await;
        Some(Image { bytes, kind })
    }

    async fn fetch_and_save(self: &Arc<Self>, url: &str, key: &str) -> Option<Image> {
        let bytes = self.fetch(url).await?;
        let kind = sniff(&bytes)?;
        let tmp = self.dir.join(format!("{key}.tmp"));
        tokio::fs::write(&tmp, &bytes).await.ok()?;
        tokio::fs::rename(&tmp, self.path(key)).await.ok()?;
        let used = self
            .used_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed)
            + bytes.len() as u64;
        if used > self.cap_bytes && !self.pruning.swap(true, Ordering::AcqRel) {
            let me = self.clone();
            tokio::task::spawn_blocking(move || {
                let left = prune(&me.dir, (me.cap_bytes as f64 * PRUNE_TO) as u64);
                me.used_bytes.store(left, Ordering::Relaxed);
                me.pruning.store(false, Ordering::Release);
            });
        }
        Some(Image { bytes, kind })
    }

    async fn fetch(&self, url: &str) -> Option<Vec<u8>> {
        let mut url = Url::parse(url).ok()?;
        for _ in 0..=MAX_REDIRECTS {
            if !hosts::allowed(&url, self.tracker.as_deref()).await {
                return None;
            }
            let resp = self.client.get(url.clone()).send().await.ok()?;
            if resp.status().is_redirection() {
                let to = resp.headers().get(LOCATION)?.to_str().ok()?;
                url = url.join(to).ok()?;
                continue;
            }
            if !resp.status().is_success() {
                return None;
            }
            let kind = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if kind.contains("html") || kind.starts_with("text/") {
                return None;
            }
            return read_capped(resp).await;
        }
        None
    }
}

/// The file name for an image: the hash of its address.
fn key(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The image type from the file's first bytes; `None` for anything else.
fn sniff(b: &[u8]) -> Option<&'static str> {
    Some(match b {
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [
            _,
            _,
            _,
            _,
            b'f',
            b't',
            b'y',
            b'p',
            b'a',
            b'v',
            b'i',
            b'f',
            ..,
        ] => "image/avif",
        [b'B', b'M', ..] => "image/bmp",
        _ => return None,
    })
}

async fn read_capped(mut resp: Response) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_IMAGE_BYTES {
            return None;
        }
    }
    Some(body)
}

/// Saved copies (not files still being written): path, size, last shown.
fn files(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().contains('.'))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| {
                (
                    e.path(),
                    m.len(),
                    m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                )
            })
        })
        .collect()
}

/// Drop the least recently shown copies until at most `target` bytes are left.
/// Returns the bytes left.
fn prune(dir: &Path, target: u64) -> u64 {
    let mut all = files(dir);
    let mut used: u64 = all.iter().map(|f| f.1).sum();
    all.sort_by_key(|f| f.2);
    for (path, size, _) in all {
        if used <= target {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            used -= size;
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    #[test]
    fn knows_image_types_by_their_first_bytes() {
        assert_eq!(sniff(PNG), Some("image/png"));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8"), Some("image/webp"));
        assert_eq!(sniff(b"\0\0\0\x1cftypavif"), Some("image/avif"));
        assert_eq!(sniff(b"<!doctype html>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[tokio::test]
    async fn serves_a_saved_copy_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::open(dir.path(), None, 1 << 20).unwrap();
        let url = "https://images.example/a.png";
        std::fs::write(dir.path().join(key(url)), PNG).unwrap();
        let img = cache.get(url).await.unwrap();
        assert_eq!(img.kind, "image/png");
        assert_eq!(img.bytes, PNG);
    }

    #[tokio::test]
    async fn never_fetches_local_or_tracker_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::open(dir.path(), Some("tracker.test".into()), 1 << 20).unwrap();
        for u in [
            "http://127.0.0.1/a.png",
            "https://tracker.test/pic.png",
            "file:///etc/passwd",
        ] {
            assert!(cache.get(u).await.is_none(), "{u} should be refused");
        }
    }

    #[test]
    fn prunes_least_recently_shown_first() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        std::fs::write(&old, [0u8; 100]).unwrap();
        std::fs::write(&new, [0u8; 100]).unwrap();
        std::fs::write(dir.path().join("x.tmp"), [0u8; 500]).unwrap();
        let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert_eq!(prune(dir.path(), 150), 100);
        assert!(!old.exists());
        assert!(new.exists());
    }
}
