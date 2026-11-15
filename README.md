# bbt

A local inbox for a private torrent tracker (set `TRACKER_BASE_URL` in `.env`). It syncs the newest uploads into
SQLite so you can sort each torrent once (Viewed, View later, Download later,
Downloaded) instead of browsing the same pages every day.

## Run

1. Copy `.env.example` to `.env` and fill in your username and password.
2. `cargo run --release -p bbt-server -- serve`
3. Open http://localhost:24781

The server only listens on the loopback addresses (127.0.0.1 and ::1).

### With Docker

1. Copy `.env.example` to `.env` and fill it in.
2. `docker compose up -d --build`
3. Open http://localhost:24781

The database lives in `./data/bbt.db`. The port is published on the loopback
addresses only, so other machines can't reach it. `docker compose logs -f` shows the
log; `docker compose down` stops it. To run without Docker against the same database,
set `BBT_DB=data/bbt.db` in `.env`.

## Keys

`j`/`k` move, `1`–`4` sort, `0` back to New, `Shift 1` mark the day's new torrents viewed (press twice above 20),
`Enter` or `l` (or a second click) details, `u` undo (again and again, up to 50 changes back), `/` search, `?` all shortcuts.
To change a key, press `?` and click it; Reset to defaults puts them all back.

`s` (or **Swipe** in the toolbar) shows new torrents one card at a time: `←` or swipe left passes (Viewed),
`↑` saves for later (View later), `→` keeps (Download later). `Space` flips screenshots, `Enter` shows one full size.
Screenshots for the card you are on and the next 2 load through the usual 3-second tracker pacing, one card at a time; change how many cards load ahead (0–5) in Settings.
Screenshots come from the description and from links in the ScreenShot row and the description's text links: for a link to an image host's page, bbt reads the picture the page names for link previews (`og:image`) and skips the page's banners and ads. Only public hosts are read, never the tracker or your own network.

Seeding, leeching and snatched counts come from the last sync that saw the torrent on a list page, so older torrents' counts go stale.
The first time a torrent's details load, its counts are updated from that page at no extra cost. **Refresh** next to Counts updated reads the page again: one tracker request, at most once a minute per torrent.

The same release is often uploaded twice under different titles. bbt reads the release code (like `MIDA-859`) from each title: a tag such as **Other upload viewed** marks the second one, and details list the other uploads with a link to see them all together.
Details also show the torrent's sorting history: every mark change, newest first.
When New is empty, **All caught up** shows a calendar of the last 12 weeks: each square is a day, darker the more torrents you sorted, with your current day streak.

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

## Commit hooks

`git config core.hooksPath .githooks` turns on the checks in `.githooks/`: commit
subjects must be `type(scope): message`, and pushes are refused for commits not
authored with the GitHub no-reply address.
