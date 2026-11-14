//! Loading older uploads one tracker page at a time, driven by the user scrolling.
//!
//! tracker lists newest first and page numbers drift as new uploads arrive: an upload
//! pushes every older row one place further back. Drift therefore only ever makes the
//! next page repeat rows already read; it never skips any. Rows can only be skipped
//! when the loader jumps more than one page, so the state remembers whether the page
//! about to be read directly follows a page that reached the gap-free boundary.

use bbt_domain::ListPage;

/// Tracker pages the user may load per day by scrolling (~2,000 uploads, ~4 days).
pub const DAILY_PAGE_BUDGET: u32 = 40;
pub const PER_PAGE: u64 = 50;

/// Where the loader stands between requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Next tracker page to read (0 = newest).
    pub page: u32,
    /// True when `page` directly follows a page that reached the boundary, so
    /// nothing can lie between them.
    pub linked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The page joined the gap-free range; the boundary moved back.
    Extended,
    /// Every row was newer than the boundary (pages drifted): move forward.
    TooNew,
    /// Every row was older than the boundary after a jump: there may be rows in
    /// between, so step back.
    Overshot,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Extended => "extended",
            Outcome::TooNew => "too_new",
            Outcome::Overshot => "overshot",
        }
    }
}

/// Result of reading one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub outcome: Outcome,
    /// Oldest upload time reached without gaps.
    pub frontier: i64,
    pub next: Cursor,
}

/// `(oldest, newest)` upload times of the non-sticky rows on a page.
fn span(page: &ListPage) -> Option<(i64, i64)> {
    let mut times = page
        .torrents
        .iter()
        .filter(|t| !t.sticky)
        .map(|t| t.uploaded_at.timestamp());
    let first = times.next()?;
    Some(times.fold((first, first), |(lo, hi), t| (lo.min(t), hi.max(t))))
}

/// Pages that `gap_secs` of uploads fill, at the rate seen on this page.
fn pages_for(gap_secs: i64, oldest: i64, newest: i64) -> u32 {
    let per_sec = PER_PAGE as f64 / (newest - oldest).max(60) as f64;
    ((gap_secs.max(0) as f64 * per_sec) / PER_PAGE as f64).floor() as u32
}

/// Decide what reading `page` at `at` means for a gap-free range ending at `frontier`.
/// Returns `None` for a page with no rows.
pub fn step(at: Cursor, page: &ListPage, frontier: i64) -> Option<Step> {
    let (oldest, newest) = span(page)?;
    Some(if oldest >= frontier {
        // Whole page is at or above the boundary. One page forward is adjacent;
        // a bigger jump (estimated from drift) is not.
        let jump = pages_for(oldest - frontier, oldest, newest).max(1);
        Step {
            outcome: Outcome::TooNew,
            frontier,
            next: Cursor {
                page: at.page + jump,
                linked: jump == 1,
            },
        }
    } else if newest >= frontier || at.linked {
        // Reaches across the boundary, or directly follows a page that did.
        Step {
            outcome: Outcome::Extended,
            frontier: oldest,
            next: Cursor {
                page: at.page + 1,
                linked: true,
            },
        }
    } else {
        let back = pages_for(frontier - newest, oldest, newest).max(1);
        Step {
            outcome: Outcome::Overshot,
            frontier,
            next: Cursor {
                page: at.page.saturating_sub(back),
                linked: false,
            },
        }
    })
}

/// First guess at the page holding the frontier: torrents we know are newer, plus
/// uploads since the last sync at the recent rate. Rounded down so we start early.
pub fn initial_page(known_newer: u64, uploads_per_sec: f64, secs_since_sync: i64) -> u32 {
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
    use std::collections::BTreeSet;

    fn row(id: u64, at: i64) -> ListedTorrent {
        ListedTorrent {
            id,
            hash_id: id,
            category: 3,
            title: "t".into(),
            poster_url: None,
            preview_url: None,
            uploaded_at: Utc.timestamp_opt(at, 0).unwrap(),
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
        }
    }

    /// Fake tracker list: newest first, one upload a minute, id = upload minute.
    struct Site {
        newest_minute: i64,
    }

    impl Site {
        fn page(&self, n: u32) -> ListPage {
            let start = self.newest_minute - i64::from(n) * PER_PAGE as i64;
            ListPage {
                torrents: (0..PER_PAGE as i64)
                    .map(|i| start - i)
                    .filter(|m| *m >= 0)
                    .map(|m| row(m as u64, m * 60))
                    .collect(),
                skipped: 0,
            }
        }
    }

    /// Read pages until the boundary passes `target` (a minute). `gap` uploads arrive
    /// before the first read (time away), then `drift` before each later one.
    /// Everything from `known_from` up is already known.
    /// Returns every id known at the end and the number of requests.
    fn walk(
        start: Cursor,
        known_from: i64,
        target: i64,
        gap: i64,
        drift: i64,
    ) -> (BTreeSet<u64>, u32) {
        let mut site = Site {
            newest_minute: 10_000,
        };
        let mut seen: BTreeSet<u64> = (known_from as u64..=10_000).collect();
        let mut at = start;
        let mut frontier = known_from * 60;
        let mut requests = 0;
        while frontier > target * 60 {
            assert!(requests < 100, "did not converge");
            site.newest_minute += if requests == 0 { gap } else { drift };
            let page = site.page(at.page);
            requests += 1;
            seen.extend(page.torrents.iter().map(|t| t.id));
            let s = step(at, &page, frontier).unwrap();
            frontier = s.frontier;
            at = s.next;
        }
        (seen, requests)
    }

    fn assert_no_gap(seen: &BTreeSet<u64>, from: i64) {
        let missing: Vec<i64> = (from..=10_000)
            .filter(|id| !seen.contains(&(*id as u64)))
            .collect();
        assert!(missing.is_empty(), "skipped ids {missing:?}");
    }

    #[test]
    fn walks_back_page_by_page() {
        // Known down to minute 9_801 (bottom of page 3); page 4 directly follows.
        let (seen, requests) = walk(
            Cursor {
                page: 4,
                linked: true,
            },
            9_801,
            9_500,
            0,
            0,
        );
        assert_no_gap(&seen, 9_500);
        assert_eq!(requests, 7);
    }

    #[test]
    fn boundary_exactly_between_pages_does_not_bounce() {
        // Unlinked start on page 3 (all at/above the boundary), page 4 all older.
        let (seen, requests) = walk(
            Cursor {
                page: 3,
                linked: false,
            },
            9_801,
            9_700,
            0,
            0,
        );
        assert_no_gap(&seen, 9_700);
        // Page 3 (all newer) → 4, 5, 6: no back-and-forth.
        assert_eq!(requests, 4);
    }

    #[test]
    fn overshoot_steps_back_and_fills_the_gap() {
        let (seen, _) = walk(
            Cursor {
                page: 9,
                linked: false,
            },
            9_801,
            9_500,
            0,
            0,
        );
        assert_no_gap(&seen, 9_500);
    }

    #[test]
    fn drift_between_reads_never_skips() {
        // Time away (any number of uploads), then a few uploads between scrolls.
        for gap in [0, 1, 7, 30, 49, 50, 51, 120, 777] {
            for drift in [0, 1, 2] {
                for linked in [true, false] {
                    let at = Cursor { page: 4, linked };
                    let (seen, _) = walk(at, 9_801, 9_000, gap, drift);
                    assert_no_gap(&seen, 9_000);
                }
            }
        }
    }

    #[test]
    fn empty_page_is_none() {
        let at = Cursor {
            page: 0,
            linked: true,
        };
        assert_eq!(step(at, &ListPage::default(), 0), None);
    }

    #[test]
    fn initial_guess_rounds_down() {
        assert_eq!(initial_page(58, 21.0 / 3600.0, 3600), 1);
        assert_eq!(initial_page(10, 0.0, 0), 0);
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
