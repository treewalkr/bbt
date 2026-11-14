use bbt_domain::{is_login_page, parse_list_page};

const PAGE: &str = include_str!("fixtures/list_page.html");

#[test]
fn parses_rows_and_skips_broken_ones() {
    let page = parse_list_page(PAGE);
    assert_eq!(page.torrents.len(), 2);
    assert_eq!(page.skipped, 1);
    assert!(!is_login_page(PAGE));
}

#[test]
fn parses_sticky_row() {
    let t = &parse_list_page(PAGE).torrents[0];
    assert_eq!(t.id, 1000017);
    assert_eq!(t.hash_id, 4200093);
    assert_eq!(t.category, 3);
    assert_eq!(t.title, "Sticky Sample Release @Tracker");
    assert!(t.sticky);
    assert_eq!(t.uploaded_at.to_rfc3339(), "2026-03-02T02:41:07+00:00");
    assert_eq!(t.size_bytes, 3_511_135_764);
    assert_eq!(t.files, 12);
    assert_eq!(t.snatched, 87);
    assert_eq!(t.seeders, 61);
    assert_eq!(t.leechers, 19);
    assert_eq!(t.free, None);
    assert_eq!(t.multiplier, None);
    assert_eq!(t.uploader_id, Some(120045));
    assert_eq!(t.uploader.as_deref(), Some("UPLOADER_A"));
    assert_eq!(t.preview_url.as_deref(), Some("https://imgbb.ws/AAAA"));
    assert!(
        t.poster_url
            .as_deref()
            .unwrap()
            .starts_with("https://images.weserv.nl/")
    );
}

#[test]
fn parses_normal_row() {
    let t = &parse_list_page(PAGE).torrents[1];
    assert_eq!(t.id, 1000342);
    assert_eq!(t.category, 5);
    assert_eq!(t.title, "Sample Movie [Full HD]");
    assert!(!t.sticky);
    assert_eq!(t.size_bytes, 721_839_718);
    assert_eq!(t.snatched, 0);
    assert_eq!(t.seeders, 0);
    assert_eq!(t.leechers, 5);
    assert_eq!(t.free.as_deref(), Some("Yes"));
    assert_eq!(t.multiplier.as_deref(), Some("x2"));
    assert_eq!(t.poster_url, None);
    assert_eq!(t.preview_url, None);
    assert_eq!(t.uploader.as_deref(), Some("UPLOADER_B"));
}
