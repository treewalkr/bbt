use bbt_domain::{DownloadGate, parse_download_gate};

const GATE: &str = r#"<html><head><title>Downloading... - -</title></head><body>
<span id="bbAdCount"></span>
<a id="bbDlBtn" class="bb-adgate-btn bb-disabled" rel="nofollow" href="/downloadnew.php?id=1000342&amp;genid=AbCdE&amp;dltm=997031&amp;dlt=0123456789abcdef&amp;filename=Sample%20Release%20580&amp;adok=1&amp;adt=1794671270.0123456789abcdef">wait</a>
<script>(function(){var s=5,b=document.getElementById("bbDlBtn");})();</script>
</body></html>"#;

#[test]
fn finds_the_button_link_and_wait() {
    assert_eq!(
        parse_download_gate(GATE),
        Some(DownloadGate {
            path: "downloadnew.php?id=1000342&genid=AbCdE&dltm=997031&dlt=0123456789abcdef&filename=Sample%20Release%20580&adok=1&adt=1794671270.0123456789abcdef".into(),
            wait_secs: 5,
        })
    );
}

#[test]
fn other_pages_are_not_a_gate() {
    assert_eq!(
        parse_download_gate("<html><body>nothing</body></html>"),
        None
    );
}

#[test]
fn wait_defaults_to_five_and_is_capped() {
    let no_count = GATE.replace("var s=5,", "");
    assert_eq!(parse_download_gate(&no_count).unwrap().wait_secs, 5);
    let long = GATE.replace("var s=5,", "var s=900,");
    assert_eq!(parse_download_gate(&long).unwrap().wait_secs, 30);
}
