//! Read queries and marking for the inbox UI.

use crate::store::Store;
use anyhow::Result;
use rusqlite::types::ToSql;
use rusqlite::{Row, params_from_iter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The user's decision about a torrent. No mark means it is new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Viewed,
    ViewLater,
    DownloadLater,
    Downloaded,
}

impl Mark {
    pub fn as_str(self) -> &'static str {
        match self {
            Mark::Viewed => "viewed",
            Mark::ViewLater => "view_later",
            Mark::DownloadLater => "download_later",
            Mark::Downloaded => "downloaded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "viewed" => Mark::Viewed,
            "view_later" => Mark::ViewLater,
            "download_later" => Mark::DownloadLater,
            "downloaded" => Mark::Downloaded,
            _ => return None,
        })
    }

    pub const ALL: [Mark; 4] = [
        Mark::Viewed,
        Mark::ViewLater,
        Mark::DownloadLater,
        Mark::Downloaded,
    ];
}

/// Which list to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    New,
    Marked(Mark),
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Newest,
    Seeders,
    Snatched,
    Leechers,
    Size,
    Rating,
}

impl Sort {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "new" => Sort::Newest,
            "seed" => Sort::Seeders,
            "snatch" => Sort::Snatched,
            "leech" => Sort::Leechers,
            "size" => Sort::Size,
            "rating" => Sort::Rating,
            _ => return None,
        })
    }

    fn order_by(self) -> &'static str {
        match self {
            Sort::Newest => "t.uploaded_at DESC",
            Sort::Seeders => "t.seeders DESC, t.uploaded_at DESC",
            Sort::Snatched => "t.snatched DESC, t.uploaded_at DESC",
            Sort::Leechers => "t.leechers DESC, t.uploaded_at DESC",
            Sort::Size => "t.size_bytes DESC, t.uploaded_at DESC",
            Sort::Rating => "r.stars IS NULL, r.stars DESC, t.uploaded_at DESC",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub view: View,
    /// Only torrents uploaded at or after this time. Applies to New and All;
    /// the marked lists always show everything in them.
    pub from: Option<i64>,
    /// Empty means every category.
    pub categories: Vec<u16>,
    pub search: Option<String>,
    pub free_only: bool,
    pub include_sticky: bool,
    pub sort: Sort,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TorrentRow {
    pub id: u64,
    pub hash_id: u64,
    pub category: u16,
    pub title: String,
    pub poster_url: Option<String>,
    pub preview_url: Option<String>,
    pub uploaded_at: i64,
    pub size_bytes: u64,
    pub files: u32,
    pub snatched: u32,
    pub seeders: u32,
    pub leechers: u32,
    pub free: Option<String>,
    pub multiplier: Option<String>,
    pub uploader: Option<String>,
    pub sticky: bool,
    pub stats_at: i64,
    pub mark: Option<Mark>,
    /// 1–5 stars, `None` when unrated.
    pub rating: Option<u8>,
    /// Release code from the title, like `MIDA-859`.
    pub release: Option<String>,
    /// Other uploads of the same release, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub others: Vec<OtherUpload>,
}

/// Another torrent with the same release code.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OtherUpload {
    pub id: u64,
    pub title: String,
    pub uploaded_at: i64,
    pub mark: Option<Mark>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    /// New, non-sticky torrents uploaded since `from`.
    pub new: u64,
    pub viewed: u64,
    pub view_later: u64,
    pub download_later: u64,
    pub downloaded: u64,
    pub total: u64,
    /// New, non-sticky torrents since `from`, per category id.
    pub new_by_category: BTreeMap<u16, u64>,
    /// Upload time of the oldest non-sticky torrent synced, if any.
    pub oldest_uploaded_at: Option<i64>,
}

pub const MAX_LIMIT: u32 = 5000;

impl Store {
    pub fn list_torrents(&self, f: &Filter) -> Result<Vec<TorrentRow>> {
        let mut sql = String::from(
            "SELECT t.id, t.category, t.title, t.poster_url, t.preview_url, t.uploaded_at,
                    t.size_bytes, t.files, t.snatched, t.seeders, t.leechers, t.free,
                    t.multiplier, t.uploader, t.sticky, t.stats_at, m.state, t.hash_id, r.stars,
                    t.release
             FROM torrents t LEFT JOIN marks m ON m.torrent_id = t.id
                             LEFT JOIN ratings r ON r.torrent_id = t.id WHERE 1=1",
        );
        let mut args: Vec<Box<dyn ToSql>> = Vec::new();
        match f.view {
            View::New => sql.push_str(" AND m.state IS NULL"),
            View::Marked(mark) => {
                sql.push_str(" AND m.state = ?");
                args.push(Box::new(mark.as_str()));
            }
            View::All => {}
        }
        if let (Some(from), View::New | View::All) = (f.from, f.view) {
            sql.push_str(" AND t.uploaded_at >= ?");
            args.push(Box::new(from));
        }
        if !f.categories.is_empty() {
            let marks = vec!["?"; f.categories.len()].join(",");
            sql.push_str(&format!(" AND t.category IN ({marks})"));
            args.extend(f.categories.iter().map(|c| Box::new(*c) as Box<dyn ToSql>));
        }
        if let Some(q) = f.search.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            sql.push_str(" AND (t.title LIKE ? ESCAPE '\\' OR t.uploader LIKE ? ESCAPE '\\')");
            let pattern = format!("%{}%", escape_like(q));
            args.push(Box::new(pattern.clone()));
            args.push(Box::new(pattern));
        }
        if f.free_only {
            sql.push_str(" AND t.free IS NOT NULL");
        }
        if !f.include_sticky {
            sql.push_str(" AND t.sticky = 0");
        }
        sql.push_str(&format!(" ORDER BY {} LIMIT ?", f.sort.order_by()));
        let limit = if f.limit == 0 {
            MAX_LIMIT
        } else {
            f.limit.min(MAX_LIMIT)
        };
        args.push(Box::new(limit));

        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(params_from_iter(args.iter()), row_to_torrent)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        self.attach_others(&mut rows)?;
        Ok(rows)
    }

    /// Fill `others` from every torrent sharing a release code with another.
    fn attach_others(&self, rows: &mut [TorrentRow]) -> Result<()> {
        if rows.iter().all(|r| r.release.is_none()) {
            return Ok(());
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT t.release, t.id, t.title, t.uploaded_at, m.state
             FROM torrents t LEFT JOIN marks m ON m.torrent_id = t.id
             WHERE t.release IN (SELECT release FROM torrents WHERE release IS NOT NULL
                                 GROUP BY release HAVING COUNT(*) > 1)
             ORDER BY t.uploaded_at, t.id",
        )?;
        let mut by_release: std::collections::HashMap<String, Vec<OtherUpload>> =
            Default::default();
        for row in stmt.query_map([], |r| {
            let state: Option<String> = r.get(4)?;
            Ok((
                r.get::<_, String>(0)?,
                OtherUpload {
                    id: r.get(1)?,
                    title: r.get(2)?,
                    uploaded_at: r.get(3)?,
                    mark: state.as_deref().and_then(Mark::parse),
                },
            ))
        })? {
            let (release, other) = row?;
            by_release.entry(release).or_default().push(other);
        }
        for row in rows {
            if let Some(all) = row.release.as_ref().and_then(|c| by_release.get(c)) {
                row.others = all.iter().filter(|o| o.id != row.id).cloned().collect();
            }
        }
        Ok(())
    }

    /// Set (or clear, with `None`) the mark on each id. Returns the previous marks
    /// for ids that exist, so the caller can undo.
    pub fn set_marks(
        &mut self,
        ids: &[u64],
        mark: Option<Mark>,
        now: i64,
    ) -> Result<Vec<(u64, Option<Mark>)>> {
        let tx = self.conn.transaction()?;
        let mut previous = Vec::with_capacity(ids.len());
        {
            let mut exists = tx.prepare_cached("SELECT 1 FROM torrents WHERE id = ?1")?;
            let mut get = tx.prepare_cached("SELECT state FROM marks WHERE torrent_id = ?1")?;
            let mut put = tx.prepare_cached(
                "INSERT INTO marks (torrent_id, state, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(torrent_id) DO UPDATE SET state = excluded.state,
                     updated_at = excluded.updated_at",
            )?;
            let mut del = tx.prepare_cached("DELETE FROM marks WHERE torrent_id = ?1")?;
            let mut log = tx.prepare_cached(
                "INSERT INTO mark_log (torrent_id, state, at) VALUES (?1, ?2, ?3)",
            )?;
            for &id in ids {
                if !exists.exists([id])? {
                    continue;
                }
                let old: Option<String> = get.query_row([id], |r| r.get(0)).ok();
                let old = old.as_deref().and_then(Mark::parse);
                previous.push((id, old));
                match mark {
                    Some(m) => put.execute(rusqlite::params![id, m.as_str(), now])?,
                    None => del.execute([id])?,
                };
                if old != mark {
                    log.execute(rusqlite::params![id, mark.map(Mark::as_str), now])?;
                }
            }
        }
        tx.commit()?;
        Ok(previous)
    }

    /// Every mark change of one torrent, newest first. `None` means back to New.
    pub fn mark_history(&self, id: u64) -> Result<Vec<(Option<Mark>, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT state, at FROM mark_log WHERE torrent_id = ?1 ORDER BY at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([id], |r| {
                let state: Option<String> = r.get(0)?;
                Ok((state.as_deref().and_then(Mark::parse), r.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// How many torrents were sorted on each day since `since`, oldest day first.
    /// Days are counted in a zone `offset` seconds east of UTC; a day is its number
    /// since 1970-01-01 there. Sending a torrent back to New doesn't count.
    pub fn sorted_per_day(&self, since: i64, offset: i64) -> Result<Vec<(i64, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT (at + ?2) / 86400 AS day, COUNT(DISTINCT torrent_id) FROM mark_log
             WHERE at >= ?1 AND state IS NOT NULL GROUP BY day ORDER BY day",
        )?;
        let rows = stmt
            .query_map([since, offset], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Set (or clear, with `None`) the star rating on each id. `stars` must be 1–5.
    /// Returns the previous ratings for ids that exist, so the caller can undo.
    pub fn set_ratings(
        &mut self,
        ids: &[u64],
        stars: Option<u8>,
        now: i64,
    ) -> Result<Vec<(u64, Option<u8>)>> {
        if let Some(s) = stars {
            anyhow::ensure!((1..=5).contains(&s), "stars must be 1 to 5, got {s}");
        }
        let tx = self.conn.transaction()?;
        let mut previous = Vec::with_capacity(ids.len());
        {
            let mut exists = tx.prepare_cached("SELECT 1 FROM torrents WHERE id = ?1")?;
            let mut get = tx.prepare_cached("SELECT stars FROM ratings WHERE torrent_id = ?1")?;
            let mut put = tx.prepare_cached(
                "INSERT INTO ratings (torrent_id, stars, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(torrent_id) DO UPDATE SET stars = excluded.stars,
                     updated_at = excluded.updated_at",
            )?;
            let mut del = tx.prepare_cached("DELETE FROM ratings WHERE torrent_id = ?1")?;
            for &id in ids {
                if !exists.exists([id])? {
                    continue;
                }
                previous.push((id, get.query_row([id], |r| r.get(0)).ok()));
                match stars {
                    Some(s) => put.execute(rusqlite::params![id, s, now])?,
                    None => del.execute([id])?,
                };
            }
        }
        tx.commit()?;
        Ok(previous)
    }

    pub fn summary(&self, from: Option<i64>) -> Result<Summary> {
        let from = from.unwrap_or(i64::MIN);
        let mut by_state: BTreeMap<String, u64> = BTreeMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT state, COUNT(*) FROM marks GROUP BY state")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))? {
            let (state, n) = row?;
            by_state.insert(state, n);
        }
        let mut new_by_category = BTreeMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT t.category, COUNT(*) FROM torrents t LEFT JOIN marks m ON m.torrent_id = t.id
             WHERE m.state IS NULL AND t.sticky = 0 AND t.uploaded_at >= ?1
             GROUP BY t.category",
        )?;
        for row in stmt.query_map([from], |r| Ok((r.get::<_, u16>(0)?, r.get(1)?)))? {
            let (cat, n) = row?;
            new_by_category.insert(cat, n);
        }
        let count = |m: Mark| by_state.get(m.as_str()).copied().unwrap_or(0);
        Ok(Summary {
            new: new_by_category.values().sum(),
            viewed: count(Mark::Viewed),
            view_later: count(Mark::ViewLater),
            download_later: count(Mark::DownloadLater),
            downloaded: count(Mark::Downloaded),
            total: self.torrent_count()?,
            new_by_category,
            oldest_uploaded_at: self.conn.query_row(
                "SELECT MIN(uploaded_at) FROM torrents WHERE sticky = 0",
                [],
                |r| r.get(0),
            )?,
        })
    }
}

impl View {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "new" => Some(View::New),
            "all" => Some(View::All),
            other => Mark::parse(other).map(View::Marked),
        }
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn row_to_torrent(r: &Row) -> rusqlite::Result<TorrentRow> {
    let state: Option<String> = r.get(16)?;
    Ok(TorrentRow {
        id: r.get(0)?,
        hash_id: r.get(17)?,
        category: r.get(1)?,
        title: r.get(2)?,
        poster_url: r.get(3)?,
        preview_url: r.get(4)?,
        uploaded_at: r.get(5)?,
        size_bytes: r.get(6)?,
        files: r.get(7)?,
        snatched: r.get(8)?,
        seeders: r.get(9)?,
        leechers: r.get(10)?,
        free: r.get(11)?,
        multiplier: r.get(12)?,
        uploader: r.get(13)?,
        sticky: r.get(14)?,
        stats_at: r.get(15)?,
        mark: state.as_deref().and_then(Mark::parse),
        rating: r.get(18)?,
        release: r.get(19)?,
        others: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bbt_domain::ListedTorrent;
    use chrono::{TimeZone, Utc};

    #[allow(clippy::too_many_arguments)]
    fn t(id: u64, cat: u16, at: i64, seeders: u32, title: &str, sticky: bool) -> ListedTorrent {
        ListedTorrent {
            id,
            hash_id: id,
            category: cat,
            title: title.into(),
            poster_url: None,
            preview_url: None,
            uploaded_at: Utc.timestamp_opt(at, 0).unwrap(),
            size_bytes: id * 10,
            files: 1,
            snatched: 0,
            seeders,
            leechers: 0,
            free: (id == 3).then(|| "Yes".into()),
            multiplier: None,
            uploader: Some("up_er".into()),
            uploader_id: None,
            sticky,
        }
    }

    fn store() -> Store {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_torrents(
            &[
                t(1, 3, 1000, 5, "Sample one", false),
                t(2, 5, 2000, 50, "Movie 100% two", false),
                t(3, 3, 3000, 1, "Sample three", false),
                t(4, 10, 10, 99, "Old sticky", true),
            ],
            5000,
        )
        .unwrap();
        s
    }

    fn ids(rows: &[TorrentRow]) -> Vec<u64> {
        rows.iter().map(|r| r.id).collect()
    }

    #[test]
    fn new_view_is_newest_first_without_sticky() {
        let s = store();
        assert_eq!(
            ids(&s.list_torrents(&Filter::default()).unwrap()),
            [3, 2, 1]
        );
        let with_sticky = Filter {
            include_sticky: true,
            ..Default::default()
        };
        assert_eq!(ids(&s.list_torrents(&with_sticky).unwrap()), [3, 2, 1, 4]);
    }

    #[test]
    fn filters_by_date_category_search_and_free() {
        let s = store();
        let f = |f: Filter| ids(&s.list_torrents(&f).unwrap());
        assert_eq!(
            f(Filter {
                from: Some(2000),
                ..Default::default()
            }),
            [3, 2]
        );
        assert_eq!(
            f(Filter {
                categories: vec![3],
                ..Default::default()
            }),
            [3, 1]
        );
        assert_eq!(
            f(Filter {
                search: Some("samp".into()),
                ..Default::default()
            }),
            [3, 1]
        );
        assert_eq!(
            f(Filter {
                search: Some("100%".into()),
                ..Default::default()
            }),
            [2]
        );
        assert_eq!(
            f(Filter {
                search: Some("up_".into()),
                ..Default::default()
            }),
            [3, 2, 1]
        );
        assert_eq!(
            f(Filter {
                free_only: true,
                ..Default::default()
            }),
            [3]
        );
    }

    #[test]
    fn sorts_and_limits() {
        let s = store();
        let by_seed = Filter {
            sort: Sort::Seeders,
            limit: 2,
            ..Default::default()
        };
        assert_eq!(ids(&s.list_torrents(&by_seed).unwrap()), [2, 1]);
    }

    #[test]
    fn marking_moves_between_lists_and_returns_previous() {
        let mut s = store();
        let prev = s.set_marks(&[1, 2, 999], Some(Mark::Viewed), 6000).unwrap();
        assert_eq!(prev, [(1, None), (2, None)]);
        assert_eq!(ids(&s.list_torrents(&Filter::default()).unwrap()), [3]);

        let prev = s.set_marks(&[2], Some(Mark::DownloadLater), 6100).unwrap();
        assert_eq!(prev, [(2, Some(Mark::Viewed))]);
        let later = Filter {
            view: View::Marked(Mark::DownloadLater),
            from: Some(99_999),
            ..Default::default()
        };
        let rows = s.list_torrents(&later).unwrap();
        assert_eq!(ids(&rows), [2], "marked lists ignore the date filter");
        assert_eq!(rows[0].mark, Some(Mark::DownloadLater));

        s.set_marks(&[2], None, 6200).unwrap();
        s.set_marks(&[2], None, 6300).unwrap(); // no change, not logged
        assert_eq!(
            s.mark_history(2).unwrap(),
            [
                (None, 6200),
                (Some(Mark::DownloadLater), 6100),
                (Some(Mark::Viewed), 6000)
            ]
        );
        assert!(s.mark_history(999).unwrap().is_empty());
        assert_eq!(ids(&s.list_torrents(&Filter::default()).unwrap()), [3, 2]);
    }

    #[test]
    fn counts_sorted_torrents_per_day() {
        let mut s = store();
        let day = 86_400;
        s.set_marks(&[1, 2], Some(Mark::Viewed), 20 * day + 100)
            .unwrap();
        // Re-sorting the same torrent that day counts it once; back to New not at all.
        s.set_marks(&[1], Some(Mark::ViewLater), 20 * day + 200)
            .unwrap();
        s.set_marks(&[3], None, 20 * day + 300).unwrap();
        s.set_marks(&[3], Some(Mark::Viewed), 22 * day - 60)
            .unwrap();
        assert_eq!(s.sorted_per_day(0, 0).unwrap(), [(20, 2), (21, 1)]);
        // Two hours east, the last mark falls on day 22.
        assert_eq!(s.sorted_per_day(0, 7200).unwrap(), [(20, 2), (22, 1)]);
        assert_eq!(s.sorted_per_day(21 * day, 0).unwrap(), [(21, 1)]);
    }

    #[test]
    fn lists_other_uploads_of_the_same_release() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_torrents(
            &[
                t(1, 3, 100, 0, "[Reducing Mosaic]MIDA-859", false),
                t(2, 3, 200, 0, "[Demosaic - 1080P] - [Mida] MIDA-859", false),
                t(3, 3, 300, 0, "Something else", false),
            ],
            400,
        )
        .unwrap();
        s.set_marks(&[1], Some(Mark::Viewed), 500).unwrap();
        let rows = s.list_torrents(&Filter::default()).unwrap();
        let by_id = |id: u64| rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(by_id(2).release.as_deref(), Some("MIDA-859"));
        let others: Vec<(u64, Option<Mark>)> =
            by_id(2).others.iter().map(|o| (o.id, o.mark)).collect();
        assert_eq!(others, [(1, Some(Mark::Viewed))]);
        assert!(by_id(3).others.is_empty() && by_id(3).release.is_none());
        // Only New is listed, but the viewed upload still shows as the other one.
        let new = s
            .list_torrents(&Filter {
                view: View::New,
                ..Filter::default()
            })
            .unwrap();
        assert_eq!(new.iter().find(|r| r.id == 2).unwrap().others[0].id, 1);
    }

    #[test]
    fn rates_independently_of_marks_and_sorts_by_rating() {
        let mut s = store();
        s.set_marks(&[1], Some(Mark::Downloaded), 6000).unwrap();
        assert_eq!(
            s.set_ratings(&[1, 999], Some(4), 6000).unwrap(),
            [(1, None)]
        );
        assert_eq!(s.set_ratings(&[2], Some(5), 6000).unwrap(), [(2, None)]);
        assert_eq!(s.set_ratings(&[1], Some(2), 6100).unwrap(), [(1, Some(4))]);
        assert!(s.set_ratings(&[1], Some(6), 6100).is_err());
        assert!(s.set_ratings(&[1], Some(0), 6100).is_err());

        let all = Filter {
            view: View::All,
            sort: Sort::Rating,
            ..Default::default()
        };
        let rows = s.list_torrents(&all).unwrap();
        assert_eq!(
            ids(&rows),
            [2, 1, 3],
            "rated first, best first, then newest"
        );
        assert_eq!(
            (rows[1].rating, rows[1].mark),
            (Some(2), Some(Mark::Downloaded))
        );

        s.set_ratings(&[2], None, 6200).unwrap();
        assert_eq!(s.list_torrents(&all).unwrap()[0].id, 1);
    }

    #[test]
    fn summarises_counts() {
        let mut s = store();
        s.set_marks(&[1], Some(Mark::Viewed), 6000).unwrap();
        s.set_marks(&[2], Some(Mark::Downloaded), 6000).unwrap();
        let sum = s.summary(None).unwrap();
        assert_eq!(
            (sum.new, sum.viewed, sum.downloaded, sum.total),
            (1, 1, 1, 4)
        );
        assert_eq!(sum.new_by_category, BTreeMap::from([(3, 1)]));
        assert_eq!(s.summary(Some(5000)).unwrap().new, 0);
        assert_eq!(
            sum.oldest_uploaded_at,
            Some(1000),
            "sticky rows are ignored"
        );
    }

    #[test]
    fn parses_names() {
        for m in Mark::ALL {
            assert_eq!(Mark::parse(m.as_str()), Some(m));
        }
        assert_eq!(
            View::parse("view_later"),
            Some(View::Marked(Mark::ViewLater))
        );
        assert_eq!(View::parse("nope"), None);
        assert_eq!(Sort::parse("snatch"), Some(Sort::Snatched));
    }
}
