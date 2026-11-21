//! SQLite storage: torrents, the user's marks, sync history and small settings.

use anyhow::Result;
use bbt_domain::{Counts, ListedTorrent, release_code};
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

/// Cached parts of each torrent's details page (images never change; the
/// download link expires, so it is not stored).
const SCHEMA_V2: &str = "
CREATE TABLE details (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents(id),
    images     TEXT    NOT NULL,
    fetched_at INTEGER NOT NULL
);
";

/// The user's 1–5 star rating, separate from marks. No row means unrated.
const SCHEMA_V3: &str = "
CREATE TABLE ratings (
    torrent_id INTEGER PRIMARY KEY REFERENCES torrents(id),
    stars      INTEGER NOT NULL CHECK (stars BETWEEN 1 AND 5),
    updated_at INTEGER NOT NULL
);
";

/// Details saved before screenshot links were followed are missing those
/// screenshots; forget them so each is fetched again the next time it is opened.
const SCHEMA_V4: &str = "DELETE FROM details;";

/// Same again for details saved before description links were followed.
const SCHEMA_V5: &str = "DELETE FROM details;";

/// Every mark change, so "did I already sort this?" can be answered later.
/// `state` is NULL when the torrent went back to New. Starts with today's marks.
const SCHEMA_V6: &str = "
CREATE TABLE mark_log (
    id         INTEGER PRIMARY KEY,
    torrent_id INTEGER NOT NULL REFERENCES torrents(id),
    state      TEXT,
    at         INTEGER NOT NULL
);
CREATE INDEX mark_log_torrent ON mark_log (torrent_id, at);
INSERT INTO mark_log (torrent_id, state, at) SELECT torrent_id, state, updated_at FROM marks;
";

/// The release code read from each title (see `bbt_domain::release_code`), so
/// other uploads of the same release can be found. Filled for old rows on upgrade.
const SCHEMA_V7: &str = "
ALTER TABLE torrents ADD COLUMN release TEXT;
CREATE INDEX torrents_release ON torrents (release);
";

/// The tracker's most seeded torrents, in its order (see `top`). Replaced on each read.
const SCHEMA_V8: &str = "
CREATE TABLE top_seeded (
    rank       INTEGER PRIMARY KEY,
    torrent_id INTEGER NOT NULL REFERENCES torrents(id)
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

/// Read the release code from every stored title.
fn fill_release_codes(conn: &Connection) -> Result<()> {
    let titles: Vec<(u64, String)> = conn
        .prepare("SELECT id, title FROM torrents")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut set = conn.prepare("UPDATE torrents SET release = ?2 WHERE id = ?1")?;
    for (id, title) in titles {
        set.execute(params![id, release_code(&title)])?;
    }
    Ok(())
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
        if version < 1 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        if version < 2 {
            conn.execute_batch(SCHEMA_V2)?;
            conn.pragma_update(None, "user_version", 2)?;
        }
        if version < 3 {
            conn.execute_batch(SCHEMA_V3)?;
            conn.pragma_update(None, "user_version", 3)?;
        }
        if version < 4 {
            conn.execute_batch(SCHEMA_V4)?;
            conn.pragma_update(None, "user_version", 4)?;
        }
        if version < 5 {
            conn.execute_batch(SCHEMA_V5)?;
            conn.pragma_update(None, "user_version", 5)?;
        }
        if version < 6 {
            conn.execute_batch(SCHEMA_V6)?;
            conn.pragma_update(None, "user_version", 6)?;
        }
        if version < 7 {
            let tx = conn.unchecked_transaction()?; // the column and its values land together
            tx.execute_batch(SCHEMA_V7)?;
            fill_release_codes(&tx)?;
            tx.pragma_update(None, "user_version", 7)?;
            tx.commit()?;
        }
        if version < 8 {
            conn.execute_batch(SCHEMA_V8)?;
            conn.pragma_update(None, "user_version", 8)?;
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
            let mut release = tx.prepare_cached("UPDATE torrents SET release=?2 WHERE id=?1")?;
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
                release.execute(params![t.id, release_code(&t.title)])?;
            }
        }
        tx.commit()?;
        Ok(new)
    }

    /// `(hash_id, title)` of a stored torrent.
    pub fn torrent_ref(&self, id: u64) -> Result<Option<(u64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT hash_id, title FROM torrents WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Live counts read from the torrent's details page.
    pub fn save_counts(&self, id: u64, c: &Counts, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE torrents SET seeders=?2, leechers=?3, snatched=?4, stats_at=?5 WHERE id=?1",
            params![id, c.seeders, c.leechers, c.snatched, now],
        )?;
        Ok(())
    }

    /// When the torrent's counts were last read, if it is known.
    pub fn stats_at(&self, id: u64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT stats_at FROM torrents WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn cached_images(&self, id: u64) -> Result<Option<Vec<String>>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT images FROM details WHERE torrent_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    pub fn save_images(&self, id: u64, images: &[String], now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO details (torrent_id, images, fetched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(torrent_id) DO UPDATE SET images = excluded.images,
                 fetched_at = excluded.fetched_at",
            params![id, serde_json::to_string(images)?, now],
        )?;
        Ok(())
    }

    /// Non-sticky torrents uploaded at or after `t`.
    pub fn count_uploaded_since(&self, t: i64) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM torrents WHERE sticky = 0 AND uploaded_at >= ?1",
            [t],
            |r| r.get(0),
        )?)
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

    /// Store the most seeded list, best first. Every id must already be a torrent.
    pub fn replace_top_seeded(&mut self, ids: &[u64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM top_seeded", [])?;
        {
            let mut insert =
                tx.prepare("INSERT INTO top_seeded (rank, torrent_id) VALUES (?1, ?2)")?;
            for (rank, id) in ids.iter().enumerate() {
                insert.execute(params![rank, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Non-sticky torrents in the most seeded list, like the other list counts.
    pub fn count_top_seeded(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM top_seeded p JOIN torrents t ON t.id = p.torrent_id
             WHERE t.sticky = 0",
            [],
            |r| r.get(0),
        )?)
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
    fn caches_images() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_torrents(&[torrent(1, 5)], 100).unwrap();
        assert_eq!(s.torrent_ref(1).unwrap(), Some((2, "Sample 1".into())));
        assert_eq!(s.torrent_ref(9).unwrap(), None);
        assert_eq!(s.cached_images(1).unwrap(), None);
        s.save_images(1, &["https://a/1.jpg".into()], 200).unwrap();
        assert_eq!(
            s.cached_images(1).unwrap(),
            Some(vec!["https://a/1.jpg".into()])
        );
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
