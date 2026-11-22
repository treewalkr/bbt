//! Screenshot links that lead to an image host's page rather than to the image.
//! Each page is read once, when its torrent's details are first fetched, and only
//! the picture the page names for link previews is kept.

use bbt_domain::page_image;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use reqwest::{Client, Response, Url, redirect};
use std::net::IpAddr;
use std::time::Duration;
use tokio::task::JoinSet;

/// At most this many links per torrent are followed.
const MAX_LINKS: usize = 8;
/// The preview tags sit in the page head; the rest of a long page is not read.
const MAX_PAGE_BYTES: usize = 1 << 20;
const MAX_REDIRECTS: usize = 3;

/// The image behind each link, in the links' order. Links that fail are skipped.
/// Links to the tracker itself are never followed (that would be a tracker request).
pub async fn resolve(links: &[String], tracker_base: &str) -> Vec<String> {
    if links.is_empty() {
        return Vec::new();
    }
    let tracker = Url::parse(tracker_base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
    let Ok(client) = Client::builder()
        .redirect(redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (compatible; bbt)")
        .build()
    else {
        return Vec::new();
    };
    let links: Vec<String> = links.iter().take(MAX_LINKS).cloned().collect();
    let mut found = vec![None; links.len()];
    let mut tasks = JoinSet::new();
    for (i, link) in links.into_iter().enumerate() {
        let (client, tracker) = (client.clone(), tracker.clone());
        tasks.spawn(async move { (i, image_behind(&client, &link, tracker.as_deref()).await) });
    }
    while let Some(done) = tasks.join_next().await {
        if let Ok((i, image)) = done {
            found[i] = image;
        }
    }
    found.into_iter().flatten().collect()
}

async fn image_behind(client: &Client, link: &str, tracker: Option<&str>) -> Option<String> {
    let mut url = Url::parse(link).ok()?;
    for _ in 0..=MAX_REDIRECTS {
        if !allowed(&url, tracker).await {
            return None;
        }
        let resp = client.get(url.clone()).send().await.ok()?;
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
        if kind.starts_with("image/") {
            return Some(url.to_string()); // the link was the image itself
        }
        if !kind.contains("html") {
            return None;
        }
        return page_image(&String::from_utf8_lossy(&read_capped(resp).await?));
    }
    None
}

async fn read_capped(mut resp: Response) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        body.extend_from_slice(&chunk);
        if body.len() >= MAX_PAGE_BYTES {
            break;
        }
    }
    Some(body)
}

/// Only public web hosts: never this machine, the home network, the tailnet or the tracker.
pub(crate) async fn allowed(url: &Url, tracker: Option<&str>) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    if tracker.is_some_and(|t| host == t || host.ends_with(&format!(".{t}"))) {
        return false;
    }
    let port = url.port_or_known_default().unwrap_or(443);
    match tokio::net::lookup_host((host.trim_matches(['[', ']']), port)).await {
        Ok(addrs) => {
            let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
            !ips.is_empty() && ips.into_iter().all(is_public)
        }
        Err(_) => false,
    }
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b))) // shared space, used by Tailscale
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00 // unique local
                || (first & 0xffc0) == 0xfe80) // link local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.109",
            "172.16.0.1",
            "169.254.1.1",
            "100.103.129.58",
            "0.0.0.0",
            "::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip} should be refused");
        }
        for ip in ["8.8.8.8", "104.21.3.4", "2606:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip} should be allowed");
        }
    }

    #[tokio::test]
    async fn refuses_local_and_tracker_hosts() {
        let tracker = Some("tracker.test");
        for u in [
            "http://localhost:24781/x",
            "http://127.0.0.1/x",
            "http://[::1]/x",
            "file:///etc/passwd",
            "https://tracker.test/pic.gif",
            "https://www.tracker.test/a",
        ] {
            assert!(
                !allowed(&Url::parse(u).unwrap(), tracker).await,
                "{u} should be refused"
            );
        }
    }
}
