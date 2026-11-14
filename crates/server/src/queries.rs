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
}

impl Sort {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "new" => Sort::Newest,
            "seed" => Sort::Seeders,
            "snatch" => Sort::Snatched,
            "leech" => Sort::Leechers,
            "size" => Sort::Size,
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
}

pub const MAX_LIMIT: u32 = 2000;

impl Store {
    pub fn list_torrents(&self, f: &Filter) -> Result<Vec<TorrentRow>> {
        let mut sql = String::from(
            "SELECT t.id, t.category, t.title, t.poster_url, t.preview_url, t.uploaded_at,
                    t.size_bytes, t.files, t.snatched, t.seeders, t.leechers, t.free,
                    t.multiplier, t.uploader, t.sticky, t.stats_at, m.state
             FROM torrents t LEFT JOIN marks m ON m.torrent_id = t.id WHERE 1=1",
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
        let rows = stmt
            .query_map(params_from_iter(args.iter()), row_to_torrent)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
            for &id in ids {
                if !exists.exists([id])? {
                    continue;
                }
                let old: Option<String> = get.query_row([id], |r| r.get(0)).ok();
                previous.push((id, old.as_deref().and_then(Mark::parse)));
                match mark {
                    Some(m) => put.execute(rusqlite::params![id, m.as_str(), now])?,
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
        assert_eq!(ids(&s.list_torrents(&Filter::default()).unwrap()), [3, 2]);
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
