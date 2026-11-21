//! Most seeded: the tracker's own list sorted by seeders, mirrored in bbt. Opening the
//! list reads its first few pages again (a handful of requests, never more often than
//! the cooldown allows), so its order and counts match the tracker.

use crate::store::Store;
use crate::sync::PageSource;
use crate::tracker::{self, Client, Credentials, Site};
use anyhow::Result;
use bbt_domain::{ListPage, parse_list_page};

/// Pages read per refresh (about 150 torrents).
pub const PAGES: u32 = 3;
/// Minimum time between refreshes.
pub const COOLDOWN_SECS: i64 = 10 * 60;
/// When the list was last read; set on every attempt so failures wait too.
pub const ATTEMPT_KEY: &str = "top_attempt_at";
/// When the stored list was last read in full.
pub const FETCHED_KEY: &str = "top_fetched_at";

/// Seconds to wait before another refresh, if any.
pub fn cooldown_left(last_attempt: Option<i64>, now: i64) -> Option<i64> {
    let left = last_attempt? + COOLDOWN_SECS - now;
    (left > 0).then_some(left)
}

/// The tracker's seeders-sorted list.
pub struct Tracker<'a> {
    pub client: &'a mut Client,
    pub creds: &'a Credentials,
    pub site: &'a Site,
}

impl PageSource for Tracker<'_> {
    async fn fetch_page(&mut self, n: u32) -> tracker::Result<ListPage> {
        let html = self
            .client
            .fetch_html(&self.site.top_seeded_page(n), self.creds)
            .await?;
        Ok(parse_list_page(&html))
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done { pages: u32, count: usize },
    CoolingDown { seconds_left: i64 },
}

/// Read the first pages of the seeders-sorted list, refresh every torrent on them and
/// store their order. The stored order is only replaced when every page read cleanly.
pub async fn refresh(store: &mut Store, source: &mut impl PageSource, now: i64) -> Result<Outcome> {
    let last = store.get(ATTEMPT_KEY)?.and_then(|v| v.parse().ok());
    if let Some(seconds_left) = cooldown_left(last, now) {
        return Ok(Outcome::CoolingDown { seconds_left });
    }
    store.set(ATTEMPT_KEY, &now.to_string())?;
    let mut ids: Vec<u64> = Vec::new();
    let mut pages = 0;
    for n in 0..PAGES {
        let page = source.fetch_page(n).await?;
        pages += 1;
        if page.skipped > page.torrents.len() {
            anyhow::bail!("tracker page layout not recognised");
        }
        if page.torrents.is_empty() {
            break;
        }
        store.upsert_torrents(&page.torrents, now)?;
        // Counts change while paging, so a torrent can show up twice: keep its first place.
        for t in &page.torrents {
            if !ids.contains(&t.id) {
                ids.push(t.id);
            }
        }
    }
    store.replace_top_seeded(&ids)?;
    store.set(FETCHED_KEY, &now.to_string())?;
    Ok(Outcome::Done {
        pages,
        count: ids.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::{Filter, View};
    use bbt_domain::ListedTorrent;
    use chrono::{TimeZone, Utc};

    const NOW: i64 = 1_800_000_000;

    fn torrent(id: u64, seeders: u32) -> ListedTorrent {
        ListedTorrent {
            id,
            hash_id: id,
            category: 3,
            title: format!("Sample {id}"),
            poster_url: None,
            preview_url: None,
            uploaded_at: Utc.timestamp_opt(NOW - id as i64, 0).unwrap(),
            size_bytes: 1,
            files: 1,
            snatched: 0,
            seeders,
            leechers: 0,
            free: None,
            multiplier: None,
            uploader_id: None,
            uploader: None,
            sticky: false,
        }
    }

    /// Pages of two torrents each, most seeders first; `repeat` puts page 0's last
    /// torrent on page 1 again (its count dropped between reads).
    struct Fake {
        fetched: Vec<u32>,
        pages: u32,
        repeat: bool,
        fail_on: Option<u32>,
    }

    impl Fake {
        fn new(pages: u32) -> Self {
            Self {
                fetched: vec![],
                pages,
                repeat: false,
                fail_on: None,
            }
        }
    }

    impl PageSource for Fake {
        async fn fetch_page(&mut self, n: u32) -> tracker::Result<ListPage> {
            self.fetched.push(n);
            if self.fail_on == Some(n) {
                return Err(tracker::Error::NotLoggedIn);
            }
            if n >= self.pages {
                return Ok(ListPage::default());
            }
            let first = u64::from(n) * 2 + 1;
            let mut torrents = vec![torrent(first, 100 - n), torrent(first + 1, 99 - n)];
            if self.repeat && n == 1 {
                torrents.insert(0, torrent(2, 98));
            }
            Ok(ListPage {
                torrents,
                skipped: 0,
            })
        }
    }

    fn top_ids(store: &Store) -> Vec<u64> {
        let f = Filter {
            view: View::Top,
            ..Default::default()
        };
        store
            .list_torrents(&f)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect()
    }

    #[tokio::test]
    async fn reads_a_few_pages_in_tracker_order() {
        let mut store = Store::open_in_memory().unwrap();
        let mut fake = Fake::new(99);
        let out = refresh(&mut store, &mut fake, NOW).await.unwrap();
        assert_eq!(fake.fetched, vec![0, 1, 2]);
        assert_eq!(out, Outcome::Done { pages: 3, count: 6 });
        assert_eq!(top_ids(&store), [1, 2, 3, 4, 5, 6]);
    }

    #[tokio::test]
    async fn waits_between_refreshes_and_replaces_the_list() {
        let mut store = Store::open_in_memory().unwrap();
        refresh(&mut store, &mut Fake::new(99), NOW).await.unwrap();
        let soon = refresh(&mut store, &mut Fake::new(99), NOW + 60).await;
        assert_eq!(soon.unwrap(), Outcome::CoolingDown { seconds_left: 540 });

        let mut short = Fake::new(1);
        let later = refresh(&mut store, &mut short, NOW + 600).await.unwrap();
        assert_eq!(short.fetched, vec![0, 1]); // stops at the empty page
        assert_eq!(later, Outcome::Done { pages: 2, count: 2 });
        assert_eq!(top_ids(&store), [1, 2]);
    }

    #[tokio::test]
    async fn a_torrent_on_two_pages_keeps_its_first_place() {
        let mut store = Store::open_in_memory().unwrap();
        let mut fake = Fake::new(2);
        fake.repeat = true;
        refresh(&mut store, &mut fake, NOW).await.unwrap();
        assert_eq!(top_ids(&store), [1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn a_failed_read_keeps_the_old_list_and_still_waits() {
        let mut store = Store::open_in_memory().unwrap();
        refresh(&mut store, &mut Fake::new(99), NOW).await.unwrap();
        let mut bad = Fake::new(99);
        bad.fail_on = Some(1);
        assert!(refresh(&mut store, &mut bad, NOW + 600).await.is_err());
        assert_eq!(top_ids(&store), [1, 2, 3, 4, 5, 6]);
        let again = refresh(&mut store, &mut Fake::new(99), NOW + 660).await;
        assert_eq!(again.unwrap(), Outcome::CoolingDown { seconds_left: 540 });
    }
}
