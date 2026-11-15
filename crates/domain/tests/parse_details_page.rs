use bbt_domain::{page_image, parse_details_page};

const PAGE: &str = include_str!("fixtures/details_page.html");

#[test]
fn finds_download_link_and_screenshots() {
    let d = parse_details_page(PAGE);
    assert_eq!(
        d.download_path.as_deref(),
        Some(
            "download.php?id=1000342&gid=AbCdE&tm=997031&token=0123456789abcdef&filename=Sample%20Release%20580"
        )
    );
    assert_eq!(
        d.images,
        [
            "https://cover.example.com/release-580.jpg",
            "https://s1.imgbb.ws/file/storage-sv1/AAAA.jpg",
            "https://s2.imgbb.ws/file/storage-sv2/BBBB.jpg",
        ]
    );
    assert_eq!(d.screenshot_links, ["https://imgbb.ws/AAAA"]);
}

#[test]
fn screenshot_row_keeps_every_link_once() {
    let d = parse_details_page(
        "<table><tr><td class=rowhead>Hashinfo</td><td><a href='https://x.test/no'>x</a></td></tr>\
         <tr><td class=rowhead>ScreenShot</td><td><a href='https://host.test/a'>a</a> \
         <a href='https://other.test/b'>b</a><a href='https://host.test/a'>a</a><a href='pic/x.gif'>x</a></td></tr></table>",
    );
    assert_eq!(
        d.screenshot_links,
        ["https://host.test/a", "https://other.test/b"]
    );
}

#[test]
fn page_image_reads_the_preview_tags_not_banners() {
    let page = "<html><head><meta property='og:image' content=' https://i.test/full.png '>\
        </head><body><img src='https://ads.test/casino.gif'><img src='https://i.test/full.md.png'></body></html>";
    assert_eq!(page_image(page).as_deref(), Some("https://i.test/full.png"));
    let only_link = "<link rel='image_src' href='https://i.test/b.jpg'>";
    assert_eq!(
        page_image(only_link).as_deref(),
        Some("https://i.test/b.jpg")
    );
    assert_eq!(page_image("<img src='https://ads.test/banner.gif'>"), None);
    assert_eq!(
        page_image("<meta property='og:image' content='/rel.png'>"),
        None
    );
}

#[test]
fn empty_when_nothing_found() {
    let d = parse_details_page("<html><body>nothing</body></html>");
    assert_eq!(d.download_path, None);
    assert!(d.images.is_empty());
    assert!(d.screenshot_links.is_empty());
}
