//! Types and HTML parsing for tracker.example's torrent list (`browse.php`).

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use scraper::{ElementRef, Html, Selector};

/// tracker shows times in Thai time (UTC+7).
const ICT_OFFSET_SECS: i32 = 7 * 3600;

/// One row of the torrent list.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedTorrent {
    pub id: u64,
    pub hash_id: u64,
    pub category: u16,
    pub title: String,
    pub poster_url: Option<String>,
    pub preview_url: Option<String>,
    pub uploaded_at: DateTime<Utc>,
    pub size_bytes: u64,
    pub files: u32,
    pub snatched: u32,
    pub seeders: u32,
    pub leechers: u32,
    /// Free-download promo text, `None` when the site shows "No".
    pub free: Option<String>,
    /// Upload multiplier promo text, `None` when the site shows "No".
    pub multiplier: Option<String>,
    pub uploader_id: Option<u64>,
    pub uploader: Option<String>,
    pub sticky: bool,
}

/// Result of parsing one list page.
#[derive(Debug, Default)]
pub struct ListPage {
    pub torrents: Vec<ListedTorrent>,
    /// Rows that looked like torrents but could not be parsed.
    pub skipped: usize,
}

/// True when tracker served its login form instead of the requested page.
pub fn is_login_page(html: &str) -> bool {
    html.contains("action=\"takelogin.php\"")
}

/// Parse every torrent row (`<tr data-category-id=…>`) on a list page.
pub fn parse_list_page(html: &str) -> ListPage {
    let doc = Html::parse_document(html);
    let rows = sel("tr[data-category-id]");
    let mut page = ListPage::default();
    for row in doc.select(&rows) {
        match parse_row(row) {
            Some(t) => page.torrents.push(t),
            None => page.skipped += 1,
        }
    }
    page
}

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

fn parse_row(row: ElementRef) -> Option<ListedTorrent> {
    let category = row.value().attr("data-category-id")?.parse().ok()?;
    let cells: Vec<ElementRef> = row
        .children()
        .filter_map(ElementRef::wrap)
        .filter(|e| e.value().name() == "td")
        .collect();
    if cells.len() < 12 {
        return None;
    }

    let link = cells[2].select(&sel("a[href^='details.php?id=']")).next()?;
    let href = link.value().attr("href")?;
    let id = query_param(href, "id")?.parse().ok()?;
    let hash_id = query_param(href, "hash_id")?.parse().ok()?;
    let title = link
        .select(&sel("b"))
        .last()
        .map(text)
        .unwrap_or_else(|| text(link).trim_start_matches("Sticky:").trim().to_string());
    if title.is_empty() {
        return None;
    }

    let poster_url = cells[1]
        .select(&sel("img"))
        .next()
        .and_then(|i| i.value().attr("src"))
        .filter(|s| s.starts_with("http"))
        .map(str::to_string);
    let preview_url = cells[2]
        .select(&sel("a.preview-btn"))
        .next()
        .and_then(|a| a.value().attr("href"))
        .map(str::to_string);
    let sticky = cells[2]
        .select(&sel("img[src*='sticky.gif']"))
        .next()
        .is_some();

    let uploader_link = cells[11]
        .select(&sel("a[href^='userdetails.php?id=']"))
        .next();

    Some(ListedTorrent {
        id,
        hash_id,
        category,
        title,
        poster_url,
        preview_url,
        uploaded_at: parse_uploaded_at(&text(cells[6]))?,
        size_bytes: parse_size(&text(cells[7]))?,
        files: first_number(&text(cells[5]))?,
        snatched: first_number(&text(cells[8]))?,
        seeders: first_number(&text(cells[9]))?,
        leechers: first_number(&text(cells[10]))?,
        free: promo(&text(cells[3])),
        multiplier: promo(&text(cells[4])),
        uploader_id: uploader_link
            .and_then(|a| a.value().attr("href"))
            .and_then(|h| query_param(h, "id"))
            .and_then(|v| v.parse().ok()),
        uploader: uploader_link.map(text).filter(|s| !s.is_empty()),
        sticky,
    })
}

/// Visible text with whitespace collapsed to single spaces.
fn text(e: ElementRef) -> String {
    e.text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn query_param<'a>(href: &'a str, key: &str) -> Option<&'a str> {
    let query = href.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

fn first_number(s: &str) -> Option<u32> {
    let digits: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn promo(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && !s.eq_ignore_ascii_case("no")).then(|| s.to_string())
}

/// Parse `"02-03-2026 09:41:07"` (Thai time) into UTC.
pub fn parse_uploaded_at(s: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(s.trim(), "%d-%m-%Y %H:%M:%S").ok()?;
    let ict = FixedOffset::east_opt(ICT_OFFSET_SECS)?;
    Some(
        ict.from_local_datetime(&naive)
            .single()?
            .with_timezone(&Utc),
    )
}

/// Parse `"3.27 GB"` / `"688.4 MB"` into bytes (1024-based).
pub fn parse_size(s: &str) -> Option<u64> {
    let (num, unit) = s.trim().split_once(' ')?;
    let n: f64 = num.replace(',', "").parse().ok()?;
    let mult: f64 = match unit.trim().to_ascii_uppercase().as_str() {
        "B" | "BYTES" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024f64.powi(2),
        "GB" => 1024f64.powi(3),
        "TB" => 1024f64.powi(4),
        _ => return None,
    };
    Some((n * mult).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1 KB"), Some(1024));
        assert_eq!(parse_size("1.5 MB"), Some(1_572_864));
        assert_eq!(parse_size("2 GB"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("1,024 B"), Some(1024));
        assert_eq!(parse_size("5 PB"), None);
        assert_eq!(parse_size("junk"), None);
    }

    #[test]
    fn converts_thai_time_to_utc() {
        let t = parse_uploaded_at("14-11-2026 07:30:00").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-11-14T00:30:00+00:00");
        assert!(parse_uploaded_at("2026-11-14").is_none());
    }

    #[test]
    fn reads_first_number() {
        assert_eq!(first_number("424 คน"), Some(424));
        assert_eq!(first_number(" 1,234 "), Some(1234));
        assert_eq!(first_number("คน"), None);
    }

    #[test]
    fn treats_no_as_no_promo() {
        assert_eq!(promo("No"), None);
        assert_eq!(promo(""), None);
        assert_eq!(promo("x2"), Some("x2".into()));
    }

    #[test]
    fn detects_login_page() {
        assert!(is_login_page(
            r#"<form method="post" action="takelogin.php">"#
        ));
        assert!(!is_login_page("<html>list</html>"));
    }
}
