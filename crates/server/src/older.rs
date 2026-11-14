//! Loading older uploads one tracker page at a time, driven by the user scrolling.
//!
//! tracker lists newest first, so page numbers drift as new uploads arrive. The cursor
//! remembers the last page read; each step classifies the page against the frontier
//! (oldest synced upload) and moves the cursor so pages are never skipped: drift only
//! causes overlap, which is harmless.

use bbt_domain::ListPage;

/// Tracker pages the user may load per day by scrolling (~2,000 uploads, ~4 days).
pub const DAILY_PAGE_BUDGET: u32 = 40;
pub const PER_PAGE: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Page reaches past the frontier: progress.
    Overlap,
    /// Whole page is newer than the frontier (pages drifted): jump forward.
    Newer,
    /// Whole page is older than the frontier (jumped too far): step back.
    Older,
}

/// `(oldest, newest)` upload times of the non-sticky rows on a page.
fn span(page: &ListPage) -> Option<(i64, i64)> {
    let times = page
        .torrents
        .iter()
        .filter(|t| !t.sticky)
        .map(|t| t.uploaded_at.timestamp());
    let (mut lo, mut hi) = (i64::MAX, i64::MIN);
    for t in times {
        lo = lo.min(t);
        hi = hi.max(t);
    }
    (lo <= hi).then_some((lo, hi))
}

pub fn classify(page: &ListPage, frontier: i64) -> Option<Fit> {
    let (oldest, newest) = span(page)?;
    Some(if oldest >= frontier {
        Fit::Newer
    } else if newest < frontier {
        Fit::Older
    } else {
        Fit::Overlap
    })
}

/// Where to read next, given the page just read at `cursor`.
pub fn next_cursor(cursor: u32, fit: Fit, page: &ListPage, frontier: i64) -> u32 {
    let pages_between = |gap_secs: i64| -> u32 {
        let Some((oldest, newest)) = span(page) else {
            return 1;
        };
        // Upload rate seen on this page, in uploads per second.
        let rate = PER_PAGE as f64 / (newest - oldest).max(60) as f64;
        ((gap_secs.max(0) as f64 * rate) / PER_PAGE as f64).floor() as u32
    };
    match fit {
        Fit::Overlap => cursor + 1,
        Fit::Newer => {
            let (oldest, _) = span(page).unwrap_or((frontier, frontier));
            cursor + pages_between(oldest - frontier).max(1)
        }
        Fit::Older => {
            let (_, newest) = span(page).unwrap_or((frontier, frontier));
            cursor.saturating_sub(pages_between(frontier - newest).max(1))
        }
    }
}

/// First guess at the page holding the frontier: torrents we know are newer, plus
/// uploads since the last sync at the recent rate. Rounded down so we start early.
pub fn initial_cursor(known_newer: u64, uploads_per_sec: f64, secs_since_sync: i64) -> u32 {
    let since = (uploads_per_sec * secs_since_sync.max(0) as f64) as u64;
    ((known_newer + since) / PER_PAGE) as u32
}

/// Pages still allowed today given `used` stored as `"YYYY-MM-DD:count"`.
pub fn budget_left(stored: Option<&str>, today: &str) -> u32 {
    let used = stored
        .and_then(|s| s.split_once(':'))
        .filter(|(day, _)| *day == today)
        .and_then(|(_, n)| n.parse::<u32>().ok())
        .unwrap_or(0);
    DAILY_PAGE_BUDGET.saturating_sub(used)
}

pub fn budget_after_use(stored: Option<&str>, today: &str) -> String {
    let used = DAILY_PAGE_BUDGET - budget_left(stored, today);
    format!("{today}:{}", used + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bbt_domain::ListedTorrent;
    use chrono::{TimeZone, Utc};

    /// 50 rows, one per minute, newest at `newest`.
    fn page(newest: i64) -> ListPage {
        ListPage {
            torrents: (0..50)
                .map(|i| ListedTorrent {
                    id: i,
                    hash_id: i,
                    category: 3,
                    title: "t".into(),
                    poster_url: None,
                    preview_url: None,
                    uploaded_at: Utc.timestamp_opt(newest - i as i64 * 60, 0).unwrap(),
                    size_bytes: 1,
                    files: 1,
                    snatched: 0,
                    seeders: 0,
                    leechers: 0,
                    free: None,
                    multiplier: None,
                    uploader_id: None,
                    uploader: None,
                    sticky: false,
                })
                .collect(),
            skipped: 0,
        }
    }

    const F: i64 = 1_000_000;

    #[test]
    fn classifies_against_frontier() {
        assert_eq!(classify(&page(F + 600), F), Some(Fit::Overlap));
        assert_eq!(classify(&page(F + 10_000), F), Some(Fit::Newer));
        assert_eq!(classify(&page(F - 60), F), Some(Fit::Older));
        assert_eq!(classify(&ListPage::default(), F), None);
    }

    #[test]
    fn overlap_steps_one_page() {
        assert_eq!(next_cursor(4, Fit::Overlap, &page(F + 600), F), 5);
    }

    #[test]
    fn newer_jumps_by_estimated_drift() {
        // Oldest row is 2h (≈ 120 uploads ≈ 2.4 pages) above the frontier.
        let p = page(F + 49 * 60 + 7200);
        assert_eq!(next_cursor(3, Fit::Newer, &p, F), 5);
        // Small drift still moves at least one page.
        assert_eq!(next_cursor(3, Fit::Newer, &page(F + 49 * 60 + 60), F), 4);
    }

    #[test]
    fn older_steps_back_without_underflow() {
        assert_eq!(next_cursor(10, Fit::Older, &page(F - 7200), F), 8);
        assert_eq!(next_cursor(0, Fit::Older, &page(F - 7200), F), 0);
    }

    #[test]
    fn initial_guess_rounds_down() {
        assert_eq!(initial_cursor(58, 21.0 / 3600.0, 3600), 1);
        assert_eq!(initial_cursor(10, 0.0, 0), 0);
    }

    #[test]
    fn daily_budget_resets_each_day() {
        assert_eq!(budget_left(None, "2026-11-14"), 40);
        assert_eq!(budget_left(Some("2026-11-14:39"), "2026-11-14"), 1);
        assert_eq!(budget_left(Some("2026-11-14:45"), "2026-11-14"), 0);
        assert_eq!(budget_left(Some("2026-11-13:40"), "2026-11-14"), 40);
        assert_eq!(
            budget_after_use(Some("2026-11-14:3"), "2026-11-14"),
            "2026-11-14:4"
        );
        assert_eq!(
            budget_after_use(Some("2026-11-13:9"), "2026-11-14"),
            "2026-11-14:1"
        );
    }
}
