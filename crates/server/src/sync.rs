//! Incremental sync of the torrent list. Reads newest pages only, with hard limits so the
//! account never looks like a scraper: page cap, request gap (in the client),
//! cooldown between syncs, and stop-on-anything-odd.

use crate::store::{Store, SyncRun};
use crate::tracker::{self, Client, Credentials, Site};
use anyhow::Result;
use bbt_domain::{ListPage, parse_list_page};

/// Absolute ceiling on pages per sync, whatever the caller asks for.
pub const HARD_MAX_PAGES: u32 = 12;
/// First sync looks back this far.
pub const FIRST_SYNC_LOOKBACK_SECS: i64 = 24 * 3600;
/// Later syncs re-read this much before the previous sync started.
pub const OVERLAP_SECS: i64 = 3600;
/// Minimum time between sync attempts.
pub const COOLDOWN_SECS: i64 = 10 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// Reached torrents older than the cutoff: everything new is saved.
    ReachedCutoff,
    /// Hit the page cap before reaching the cutoff: older items were skipped.
    PageCap,
    /// A page had no torrents.
    EmptyPage,
    /// Rows were present but most could not be parsed: the site layout changed.
    LayoutChanged,
}

impl Stop {
    pub fn describe(self) -> &'static str {
        match self {
            Stop::ReachedCutoff => "reached cutoff",
            Stop::PageCap => "page cap reached, older items skipped",
            Stop::EmptyPage => "empty page",
            Stop::LayoutChanged => "page layout not recognised",
        }
    }
}

/// Where sync stops reading: a bit before the previous sync, or a day back on first run.
pub fn cutoff(last: Option<&SyncRun>, now: i64) -> i64 {
    match last {
        Some(run) => run.started_at - OVERLAP_SECS,
        None => now - FIRST_SYNC_LOOKBACK_SECS,
    }
}

/// Seconds to wait before another sync is allowed, if any.
pub fn cooldown_left(last_attempt: Option<i64>, now: i64) -> Option<i64> {
    let left = last_attempt? + COOLDOWN_SECS - now;
    (left > 0).then_some(left)
}

/// Decide whether to stop after reading `pages_read` pages, the last being `page`.
pub fn stop_after(page: &ListPage, cutoff: i64, pages_read: u32, max_pages: u32) -> Option<Stop> {
    if page.skipped > page.torrents.len() {
        return Some(Stop::LayoutChanged);
    }
    let oldest = page
        .torrents
        .iter()
        .filter(|t| !t.sticky)
        .map(|t| t.uploaded_at.timestamp())
        .min();
    match oldest {
        None => Some(Stop::EmptyPage),
        Some(t) if t < cutoff => Some(Stop::ReachedCutoff),
        Some(_) if pages_read >= max_pages.min(HARD_MAX_PAGES) => Some(Stop::PageCap),
        Some(_) => None,
    }
}

/// Something that can fetch list page `n` (0 = newest).
pub trait PageSource {
    fn fetch_page(&mut self, n: u32) -> impl Future<Output = tracker::Result<ListPage>>;
}

/// The real site: logs in when needed and retries once if the session expired.
pub struct Tracker<'a> {
    pub client: &'a mut Client,
    pub creds: &'a Credentials,
    pub site: &'a Site,
}

impl PageSource for Tracker<'_> {
    async fn fetch_page(&mut self, n: u32) -> tracker::Result<ListPage> {
        let html = self
            .client
            .fetch_html(&self.site.list_page(n), self.creds)
            .await?;
        Ok(parse_list_page(&html))
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done(Report),
    CoolingDown { seconds_left: i64 },
}

#[derive(Debug, PartialEq)]
pub struct Report {
    pub pages: u32,
    pub new_count: u32,
    pub seen: u32,
    pub stop: Stop,
}

/// Run one sync. Errors are recorded in the sync run before being returned.
pub async fn run(
    store: &mut Store,
    source: &mut impl PageSource,
    max_pages: u32,
    now: i64,
) -> Result<Outcome> {
    if let Some(seconds_left) = cooldown_left(store.last_sync_attempt()?, now) {
        return Ok(Outcome::CoolingDown { seconds_left });
    }
    let cutoff = cutoff(store.last_sync()?.as_ref(), now);
    let run_id = store.start_sync(now, cutoff)?;
    let (mut pages, mut new_count, mut seen) = (0, 0, 0);
    let result: Result<Stop> = async {
        for n in 0..HARD_MAX_PAGES {
            let page = source.fetch_page(n).await?;
            pages += 1;
            seen += page.torrents.len() as u32;
            new_count += store.upsert_torrents(&page.torrents, now)?;
            if let Some(stop) = stop_after(&page, cutoff, pages, max_pages) {
                return Ok(stop);
            }
        }
        Ok(Stop::PageCap)
    }
    .await;
    match result {
        Ok(stop) => {
            store.finish_sync(run_id, now, pages, new_count, stop.describe())?;
            Ok(Outcome::Done(Report {
                pages,
                new_count,
                seen,
                stop,
            }))
        }
        Err(e) => {
            store.finish_sync(run_id, now, pages, new_count, &format!("error: {e}"))?;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bbt_domain::ListedTorrent;
    use chrono::{TimeZone, Utc};

    const NOW: i64 = 1_800_000_000;

    fn torrent(id: u64, uploaded_at: i64, sticky: bool) -> ListedTorrent {
        ListedTorrent {
            id,
            hash_id: id,
            category: 3,
            title: format!("Sample {id}"),
            poster_url: None,
            preview_url: None,
            uploaded_at: Utc.timestamp_opt(uploaded_at, 0).unwrap(),
            size_bytes: 1,
            files: 1,
            snatched: 0,
            seeders: 0,
            leechers: 0,
            free: None,
            multiplier: None,
            uploader_id: None,
            uploader: None,
            sticky,
        }
    }

    /// Endless fake list: 50 torrents per page, one per minute going back,
    /// plus an old sticky on page 0.
    struct Fake {
        fetched: Vec<u32>,
    }

    impl PageSource for Fake {
        async fn fetch_page(&mut self, n: u32) -> tracker::Result<ListPage> {
            self.fetched.push(n);
            let mut torrents: Vec<_> = (0..50u64)
                .map(|i| {
                    let idx = u64::from(n) * 50 + i;
                    torrent(10_000 - idx, NOW - (idx as i64) * 60, false)
                })
                .collect();
            if n == 0 {
                torrents.insert(0, torrent(1, NOW - 30 * 86_400, true));
            }
            Ok(ListPage {
                torrents,
                skipped: 0,
            })
        }
    }

    #[test]
    fn first_sync_looks_back_one_day_then_overlaps() {
        assert_eq!(cutoff(None, NOW), NOW - 86_400);
        let last = SyncRun {
            started_at: NOW - 600,
            finished_at: Some(NOW - 590),
            pages: 1,
            new_count: 1,
            stop_reason: None,
        };
        assert_eq!(cutoff(Some(&last), NOW), NOW - 600 - 3600);
    }

    #[test]
    fn cooldown_blocks_quick_repeat() {
        assert_eq!(cooldown_left(None, NOW), None);
        assert_eq!(cooldown_left(Some(NOW - 60), NOW), Some(540));
        assert_eq!(cooldown_left(Some(NOW - 600), NOW), None);
    }

    #[test]
    fn sticky_rows_do_not_trigger_cutoff() {
        let page = ListPage {
            torrents: vec![torrent(1, NOW - 99_999, true), torrent(2, NOW, false)],
            skipped: 0,
        };
        assert_eq!(stop_after(&page, NOW - 10, 1, 5), None);
    }

    #[test]
    fn stops_on_odd_pages() {
        let empty = ListPage::default();
        assert_eq!(stop_after(&empty, 0, 1, 5), Some(Stop::EmptyPage));
        let broken = ListPage {
            torrents: vec![torrent(1, NOW, false)],
            skipped: 40,
        };
        assert_eq!(stop_after(&broken, 0, 1, 5), Some(Stop::LayoutChanged));
    }

    #[tokio::test]
    async fn never_reads_more_than_hard_cap() {
        let mut store = Store::open_in_memory().unwrap();
        let mut fake = Fake { fetched: vec![] };
        let out = run(&mut store, &mut fake, 999, NOW).await.unwrap();
        assert_eq!(fake.fetched, (0..12).collect::<Vec<_>>());
        let Outcome::Done(report) = out else {
            panic!("expected a sync")
        };
        assert_eq!(report.stop, Stop::PageCap);
        assert_eq!(report.new_count, 601);
    }

    #[tokio::test]
    async fn respects_requested_page_limit() {
        let mut store = Store::open_in_memory().unwrap();
        let mut fake = Fake { fetched: vec![] };
        run(&mut store, &mut fake, 1, NOW).await.unwrap();
        assert_eq!(fake.fetched, vec![0]);
    }

    #[tokio::test]
    async fn second_sync_stops_at_cutoff_and_respects_cooldown() {
        let mut store = Store::open_in_memory().unwrap();
        let mut fake = Fake { fetched: vec![] };
        run(&mut store, &mut fake, 5, NOW).await.unwrap();

        let too_soon = run(&mut store, &mut fake, 5, NOW + 60).await.unwrap();
        assert_eq!(too_soon, Outcome::CoolingDown { seconds_left: 540 });

        // 2h later: cutoff = first start - 1h; page 0 ends 49 min back, page 1 ends 99 min back.
        fake.fetched.clear();
        let later = run(&mut store, &mut fake, 5, NOW + 7200).await.unwrap();
        assert_eq!(fake.fetched, vec![0, 1]);
        let Outcome::Done(report) = later else {
            panic!("expected a sync")
        };
        assert_eq!(report.stop, Stop::ReachedCutoff);
        assert_eq!(report.new_count, 0);
    }
}
