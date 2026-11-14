# bbt

A local inbox for a private torrent tracker (set `TRACKER_BASE_URL` in `.env`). It syncs the newest uploads into
SQLite so you can sort each torrent once (Viewed, View later, Download later,
Downloaded) instead of browsing the same pages every day.

## Run

1. Copy `.env.example` to `.env` and fill in your username and password.
2. `cargo run --release -p bbt-server -- serve`
3. Open http://localhost:4781

The server only listens on the loopback addresses (127.0.0.1 and ::1).

### With Docker

1. Copy `.env.example` to `.env` and fill it in.
2. `docker compose up -d --build`
3. Open http://localhost:4781

The database lives in `./data/bbt.db`. The port is published on the loopback
addresses only, so other machines can't reach it. `docker compose logs -f` shows the
log; `docker compose down` stops it. To run without Docker against the same database,
set `BBT_DB=data/bbt.db` in `.env`.

## Keys

`j`/`k` move, `1`–`4` sort, `0` back to New, `Shift 1` mark the whole day viewed,
`Enter` details, `u` undo, `/` search, `?` all shortcuts.

## Sync limits

Sync never crawls the archive. Each run:

- reads at most 12 list pages (about one day of uploads), 3 seconds apart
- stops at the previous sync minus 1 hour (1 day back on the first run)
- refuses to run again within 10 minutes
- stops on anything unexpected (login loop, empty or unrecognised page)

Details pages are fetched only when you open a torrent or download it.

To go further back, open **Everything**, pick a **From** date and scroll to the end:
each load reads exactly one older tracker page, up to 40 pages a day, and stops at
the From date.

`cargo run -p bbt-server -- sync [--max-pages N]` runs one sync from the terminal.
