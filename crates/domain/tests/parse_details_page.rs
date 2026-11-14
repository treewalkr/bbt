use bbt_domain::parse_details_page;

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
}

#[test]
fn empty_when_nothing_found() {
    let d = parse_details_page("<html><body>nothing</body></html>");
    assert_eq!(d.download_path, None);
    assert!(d.images.is_empty());
}
