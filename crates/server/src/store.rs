//! SQLite storage: torrents, the user's marks, sync history and small settings.

use anyhow::Result;
use bbt_domain::ListedTorrent;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE torrents (
    id            INTEGER PRIMARY KEY,
    hash_id      INTEGER NOT NULL,
    category      INTEGER NOT NULL,
    title         TEXT    NOT NULL,
    poster_url    TEXT,
    preview_url   TEXT,
    uploaded_at   INTEGER NOT NULL,
    size_bytes    INTEGER NOT NULL,
    files         INTEGER NOT NULL,
    snatched      INTEGER NOT NULL,
    seeders       INTEGER NOT NULL,
    leechers      INTEGER NOT NULL,
    free          TEXT,
    multiplier    TEXT,
    uploader_id   INTEGER,
    uploader      TEXT,
    sticky        INTEGER NOT NULL,
    first_seen_at INTEGER NOT NULL,
    stats_at      INTEGER NOT NULL
);
CREATE INDEX torrents_uploaded_at ON torrents(uploaded_at);

-- No row means the torrent is new (not sorted yet).
CREATE TABLE marks (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents(id),
    state      TEXT NOT NULL CHECK (state IN ('viewed','view_later','download_later','downloaded')),
    updated_at INTEGER NOT NULL
);

CREATE TABLE sync_runs (
    id          INTEGER PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    cutoff      INTEGER NOT NULL,
    pages       INTEGER NOT NULL DEFAULT 0,
    new_count   INTEGER NOT NULL DEFAULT 0,
    stop_reason TEXT
);

CREATE TABLE kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

pub struct Store {
    pub(crate) conn: Connection,
}

/// Summary of a finished sync run.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SyncRun {
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub pages: u32,
    pub new_count: u32,
    pub stop_reason: Option<String>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        Ok(Self { conn })
    }

    /// Insert new torrents and refresh counts on known ones. Marks are never touched.
    /// Returns how many torrents were new.
    pub fn upsert_torrents(&mut self, torrents: &[ListedTorrent], now: i64) -> Result<u32> {
        let tx = self.conn.transaction()?;
        let mut new = 0;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO torrents (id, hash_id, category, title, poster_url, preview_url,
                    uploaded_at, size_bytes, files, snatched, seeders, leechers, free, multiplier,
                    uploader_id, uploader, sticky, first_seen_at, stats_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?18)",
            )?;
            let mut update = tx.prepare_cached(
                "UPDATE torrents SET title=?2, poster_url=?3, preview_url=?4, snatched=?5, seeders=?6,
                    leechers=?7, free=?8, multiplier=?9, sticky=?10, stats_at=?11
                 WHERE id=?1",
            )?;
            for t in torrents {
                let inserted = insert.execute(params![
                    t.id,
                    t.hash_id,
                    t.category,
                    t.title,
                    t.poster_url,
                    t.preview_url,
                    t.uploaded_at.timestamp(),
                    t.size_bytes,
                    t.files,
                    t.snatched,
                    t.seeders,
                    t.leechers,
                    t.free,
                    t.multiplier,
                    t.uploader_id,
                    t.uploader,
                    t.sticky,
                    now,
                ])?;
                if inserted == 1 {
                    new += 1;
                } else {
                    update.execute(params![
                        t.id,
                        t.title,
                        t.poster_url,
                        t.preview_url,
                        t.snatched,
                        t.seeders,
                        t.leechers,
                        t.free,
                        t.multiplier,
                        t.sticky,
                        now,
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(new)
    }

    pub fn torrent_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM torrents", [], |r| r.get(0))?)
    }

    pub fn start_sync(&self, now: i64, cutoff: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO sync_runs (started_at, cutoff) VALUES (?1, ?2)",
            params![now, cutoff],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_sync(
        &self,
        run_id: i64,
        now: i64,
        pages: u32,
        new_count: u32,
        stop_reason: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_runs SET finished_at=?2, pages=?3, new_count=?4, stop_reason=?5 WHERE id=?1",
            params![run_id, now, pages, new_count, stop_reason],
        )?;
        Ok(())
    }

    /// Most recent run that finished, whatever its outcome.
    pub fn last_sync(&self) -> Result<Option<SyncRun>> {
        Ok(self
            .conn
            .query_row(
                "SELECT started_at, finished_at, pages, new_count, stop_reason FROM sync_runs
                 WHERE finished_at IS NOT NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok(SyncRun {
                        started_at: r.get(0)?,
                        finished_at: r.get(1)?,
                        pages: r.get(2)?,
                        new_count: r.get(3)?,
                        stop_reason: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Start time of the most recent sync attempt, finished or not (for the cooldown).
    pub fn last_sync_attempt(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT MAX(started_at) FROM sync_runs", [], |r| r.get(0))?)
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn torrent(id: u64, seeders: u32) -> ListedTorrent {
        ListedTorrent {
            id,
            hash_id: id + 1,
            category: 3,
            title: format!("Sample {id}"),
            poster_url: None,
            preview_url: None,
            uploaded_at: Utc.with_ymd_and_hms(2026, 11, 14, 10, 0, 0).unwrap(),
            size_bytes: 1024,
            files: 1,
            snatched: 0,
            seeders,
            leechers: 0,
            free: None,
            multiplier: None,
            uploader_id: Some(1),
            uploader: Some("up".into()),
            sticky: false,
        }
    }

    #[test]
    fn counts_only_new_torrents_and_refreshes_stats() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(
            s.upsert_torrents(&[torrent(1, 5), torrent(2, 5)], 100)
                .unwrap(),
            2
        );
        assert_eq!(
            s.upsert_torrents(&[torrent(2, 9), torrent(3, 1)], 200)
                .unwrap(),
            1
        );
        assert_eq!(s.torrent_count().unwrap(), 3);
        let (seeders, first_seen, stats_at): (u32, i64, i64) = s
            .conn
            .query_row(
                "SELECT seeders, first_seen_at, stats_at FROM torrents WHERE id=2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((seeders, first_seen, stats_at), (9, 100, 200));
    }

    #[test]
    fn keeps_marks_when_torrent_is_seen_again() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_torrents(&[torrent(1, 5)], 100).unwrap();
        s.conn
            .execute(
                "INSERT INTO marks (torrent_id, state, updated_at) VALUES (1, 'viewed', 150)",
                [],
            )
            .unwrap();
        s.upsert_torrents(&[torrent(1, 7)], 200).unwrap();
        let state: String = s
            .conn
            .query_row("SELECT state FROM marks WHERE torrent_id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "viewed");
    }

    #[test]
    fn records_sync_runs() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.last_sync().unwrap(), None);
        let id = s.start_sync(1000, 500).unwrap();
        assert_eq!(s.last_sync().unwrap(), None);
        assert_eq!(s.last_sync_attempt().unwrap(), Some(1000));
        s.finish_sync(id, 1010, 2, 40, "reached cutoff").unwrap();
        let run = s.last_sync().unwrap().unwrap();
        assert_eq!((run.pages, run.new_count), (2, 40));
        assert_eq!(run.stop_reason.as_deref(), Some("reached cutoff"));
    }

    #[test]
    fn stores_settings() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.get("k").unwrap(), None);
        s.set("k", "a").unwrap();
        s.set("k", "b").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("b"));
    }
}
