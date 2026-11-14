//! Minimal tracker HTTP client: login, cookies, polite pacing, Thai decoding.

use bbt_domain::{is_login_page, parse_download_gate};
use rand::RngExt;
use reqwest::header::{COOKIE, LOCATION, REFERER, SET_COOKIE, USER_AGENT};
use reqwest::{Response, StatusCode, redirect};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::time::{Instant, sleep, sleep_until};

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
/// Cookies that prove a logged-in session.
const AUTH_COOKIE: &str = "uid";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not logged in")]
    NotLoggedIn,
    #[error("login failed: {0}")]
    LoginFailed(String),
    #[error("unexpected response from {path}: {detail}")]
    Unexpected { path: String, detail: String },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Download {
    pub bytes: Vec<u8>,
    pub content_disposition: Option<String>,
}

enum Fetched {
    File(Download),
    Page(String),
}

fn unexpected_page(path: &str, html: &str) -> Error {
    Error::Unexpected {
        path: path.into(),
        detail: format!("got a page instead of a file: {}", summarize(html)),
    }
}

/// Names that differ between sites. They live in `.env`, so the code names no site.
#[derive(Debug, Clone)]
pub struct Site {
    /// Torrent list page, e.g. `browse.php`.
    pub list_path: String,
    /// Cookie that carries the device id.
    pub device_cookie: String,
    /// Details-page query parameter that goes with `id`.
    pub hash_param: String,
    /// Category ids and names, in display order. Empty hides categories in the UI.
    pub categories: Vec<(u32, String)>,
}

impl Default for Site {
    fn default() -> Self {
        Self {
            list_path: "browse.php".into(),
            device_cookie: "did".into(),
            hash_param: "hash_id".into(),
            categories: Vec::new(),
        }
    }
}

impl Site {
    /// List page `n` (0 = newest).
    pub fn list_page(&self, n: u32) -> String {
        if n == 0 {
            self.list_path.clone()
        } else {
            format!("{}?page={n}", self.list_path)
        }
    }

    pub fn details_page(&self, id: u64, hash_id: u64) -> String {
        self.details_template()
            .replace("{id}", &id.to_string())
            .replace("{hash_id}", &hash_id.to_string())
    }

    /// Details link with `{id}` and `{hash_id}` placeholders (the UI fills them in).
    pub fn details_template(&self) -> String {
        format!("details.php?id={{id}}&{}={{hash_id}}", self.hash_param)
    }
}

pub struct Credentials {
    pub username: String,
    pub password: String,
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    device_cookie: String,
    device_id: String,
    cookies: BTreeMap<String, String>,
    min_gap: Duration,
    next_allowed: Instant,
    requests: u32,
}

impl Client {
    /// `saved_cookies` is the value previously returned by [`Client::cookie_header`].
    /// Every request waits until at least `min_gap` has passed since the last one.
    pub fn new(
        base: &str,
        device_cookie: &str,
        device_id: &str,
        saved_cookies: Option<&str>,
        min_gap: Duration,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        let mut cookies = saved_cookies.map(parse_cookie_header).unwrap_or_default();
        cookies.insert(device_cookie.into(), device_id.into());
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string() + "/",
            device_cookie: device_cookie.into(),
            device_id: device_id.into(),
            cookies,
            min_gap,
            next_allowed: Instant::now(),
            requests: 0,
        })
    }

    /// Cookies to persist so the next run can skip logging in.
    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn has_session(&self) -> bool {
        self.cookies.contains_key(AUTH_COOKIE)
    }

    /// Number of HTTP requests sent so far.
    pub fn requests(&self) -> u32 {
        self.requests
    }

    pub async fn login(&mut self, creds: &Credentials) -> Result<()> {
        let keep = self.device_cookie.clone();
        self.cookies.retain(|k, _| *k == keep);
        let page = self.send(self.request_get("login.php")).await?;
        if !page.status().is_success() {
            return Err(Error::LoginFailed(format!(
                "login page returned {}",
                page.status()
            )));
        }
        let form = [
            ("device_id", self.device_id.as_str()),
            ("username", creds.username.as_str()),
            ("password", creds.password.as_str()),
            ("returnto", "/"),
        ];
        let req = self
            .http
            .post(self.url("takelogin.php"))
            .header(REFERER, self.url("login.php"))
            .form(&form);
        let resp = self.send(req).await?;
        if resp.status() == StatusCode::FOUND && self.has_session() {
            Ok(())
        } else {
            let status = resp.status();
            let body = decode(&resp.bytes().await.unwrap_or_default());
            Err(Error::LoginFailed(format!(
                "status {status}, {}",
                summarize(&body)
            )))
        }
    }

    /// Fetch a page as UTF-8 HTML. Redirects are not followed: a redirect to the
    /// login page means the session expired; any other redirect is unexpected.
    pub async fn get_html(&mut self, path: &str) -> Result<String> {
        let resp = self.send(self.request_get(path)).await?;
        let status = resp.status();
        if status.is_redirection() {
            let to = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            return Err(if to.contains("login.php") {
                Error::NotLoggedIn
            } else {
                Error::Unexpected {
                    path: path.into(),
                    detail: format!("redirect to {to}"),
                }
            });
        }
        if !status.is_success() {
            return Err(Error::Unexpected {
                path: path.into(),
                detail: format!("status {status}"),
            });
        }
        let html = decode(&resp.bytes().await?);
        if is_login_page(&html) {
            return Err(Error::NotLoggedIn);
        }
        Ok(html)
    }

    /// [`Client::get_html`], logging in first if needed and once more if the session expired.
    pub async fn fetch_html(&mut self, path: &str, creds: &Credentials) -> Result<String> {
        if !self.has_session() {
            self.login(creds).await?;
        }
        match self.get_html(path).await {
            Err(Error::NotLoggedIn) => {
                self.login(creds).await?;
                self.get_html(path).await
            }
            other => other,
        }
    }

    /// Download a file (a `.torrent`). Fails if tracker answers with an HTML page instead.
    /// Download a file. When the site shows its countdown page first, wait it out and
    /// follow the button, as a browser would.
    pub async fn fetch_file(&mut self, path: &str) -> Result<Download> {
        let html = match self.get_file(path, None).await? {
            Fetched::File(file) => return Ok(file),
            Fetched::Page(html) => html,
        };
        let gate = parse_download_gate(&html).ok_or_else(|| unexpected_page(path, &html))?;
        sleep(Duration::from_secs(gate.wait_secs + 1)).await;
        match self.get_file(&gate.path, Some(path)).await? {
            Fetched::File(file) => Ok(file),
            Fetched::Page(html) => Err(unexpected_page(&gate.path, &html)),
        }
    }

    /// One GET that should return a file; an HTML page comes back as [`Fetched::Page`].
    async fn get_file(&mut self, path: &str, referer: Option<&str>) -> Result<Fetched> {
        let mut req = self.request_get(path);
        if let Some(from) = referer {
            req = req.header(REFERER, self.url(from));
        }
        let resp = self.send(req).await?;
        let status = resp.status();
        let header = |name| {
            resp.headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(str::to_string)
        };
        let content_type = header(reqwest::header::CONTENT_TYPE).unwrap_or_default();
        let disposition = header(reqwest::header::CONTENT_DISPOSITION);
        if status.is_redirection() {
            return Err(Error::Unexpected {
                path: path.into(),
                detail: format!("redirect to {}", header(LOCATION).unwrap_or_default()),
            });
        }
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            return Err(Error::Unexpected {
                path: path.into(),
                detail: format!("status {status}, {}", summarize(&decode(&bytes))),
            });
        }
        if content_type.contains("html") {
            return Ok(Fetched::Page(decode(&bytes)));
        }
        Ok(Fetched::File(Download {
            bytes: bytes.to_vec(),
            content_disposition: disposition,
        }))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path.trim_start_matches('/'))
    }

    fn request_get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(self.url(path))
    }

    async fn send(&mut self, req: reqwest::RequestBuilder) -> Result<Response> {
        sleep_until(self.next_allowed).await;
        let resp = req
            .header(USER_AGENT, UA)
            .header(COOKIE, self.cookie_header())
            .send()
            .await;
        self.requests += 1;
        self.next_allowed = Instant::now() + self.min_gap;
        let resp = resp?;
        for v in resp.headers().get_all(SET_COOKIE) {
            if let Some((name, value)) = v.to_str().ok().and_then(parse_set_cookie) {
                if value.is_empty() || value == "deleted" {
                    self.cookies.remove(&name);
                } else {
                    self.cookies.insert(name, value);
                }
            }
        }
        Ok(resp)
    }
}

/// A fresh device id in the same shape the site's own script produces (sha256 hex).
pub fn generate_device_id() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    let uuid: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let raw = uuid;
    Sha256::digest(raw.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// tracker serves windows-874 (Thai).
fn decode(bytes: &[u8]) -> String {
    encoding_rs::WINDOWS_874.decode(bytes).0.into_owned()
}

fn summarize(html: &str) -> String {
    let title = html
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>"))
        .map(|(t, _)| t.trim())
        .unwrap_or("");
    if title.is_empty() {
        "no page title".into()
    } else {
        format!("page title {title:?}")
    }
}

fn parse_set_cookie(header: &str) -> Option<(String, String)> {
    let pair = header.split(';').next()?;
    let (name, value) = pair.split_once('=')?;
    Some((name.trim().to_string(), value.trim().to_string()))
}

fn parse_cookie_header(header: &str) -> BTreeMap<String, String> {
    header
        .split(';')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_set_cookie() {
        assert_eq!(
            parse_set_cookie("uid=291; expires=Tue, 19-Jan-2038 03:14:07 GMT; path=/; secure"),
            Some(("uid".into(), "291".into()))
        );
        assert_eq!(parse_set_cookie("garbage"), None);
    }

    #[test]
    fn round_trips_cookie_header() {
        let c = Client::new(
            "https://example.org/",
            "did",
            "dev",
            Some("uid=1; pass=abc"),
            Duration::ZERO,
        )
        .unwrap();
        assert!(c.has_session());
        assert_eq!(c.cookie_header(), "did=dev; pass=abc; uid=1");
        assert_eq!(parse_cookie_header(&c.cookie_header()).len(), 3);
    }

    #[test]
    fn no_session_without_uid_cookie() {
        let c = Client::new("https://example.org/", "did", "dev", None, Duration::ZERO).unwrap();
        assert!(!c.has_session());
    }

    #[test]
    fn device_id_is_sha256_hex() {
        let id = generate_device_id();
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, generate_device_id());
    }

    #[test]
    fn decodes_thai() {
        // "ไทย" in windows-874
        assert_eq!(decode(&[0xE4, 0xB7, 0xC2]), "ไทย");
    }

    #[test]
    fn builds_urls() {
        let c = Client::new("https://example.org", "did", "d", None, Duration::ZERO).unwrap();
        assert_eq!(c.url("/browse.php"), "https://example.org/browse.php");
    }

    #[test]
    fn builds_site_paths() {
        let site = Site::default();
        assert_eq!(site.list_page(0), "browse.php");
        assert_eq!(site.list_page(3), "browse.php?page=3");
        assert_eq!(site.details_page(7, 9), "details.php?id=7&hash_id=9");
    }
}
