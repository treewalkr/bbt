//! JSON API for the local inbox UI. Binds to localhost only (see `serve`).

use crate::config::{self, Config};
use crate::hosts;
use crate::images::ImageCache;
use crate::older::{self, Cursor};
use crate::queries::{Filter, Mark, Sort, View};
use crate::store::Store;
use crate::sync::{self, HARD_MAX_PAGES, Outcome};
use crate::top;
use crate::tracker::{self, Client};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bbt_domain::{Details, parse_details_page, parse_list_page};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct AppState {
    db_path: String,
    store: Mutex<Store>,
    /// Held for the whole duration of a sync so two can never overlap.
    sync_lock: Mutex<()>,
    /// `None` disables all tracker access (tests).
    config: Option<Config>,
    /// One client for sync, details and downloads, so the request gap applies to all.
    client: Mutex<Option<Client>>,
    /// Posters and screenshots, saved next to the database.
    images: Arc<ImageCache>,
}

impl AppState {
    pub fn new(db_path: &str, config: Option<Config>) -> anyhow::Result<Arc<Self>> {
        let dir = std::path::Path::new(db_path)
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("images");
        let tracker = config.as_ref().and_then(|c| {
            reqwest::Url::parse(&c.base_url)
                .ok()?
                .host_str()
                .map(str::to_ascii_lowercase)
        });
        let cap_mb = config
            .as_ref()
            .map_or(config::DEFAULT_IMAGE_CACHE_MB, |c| c.image_cache_mb);
        let images = ImageCache::open(dir, tracker, cap_mb << 20)?;
        Ok(Arc::new(Self {
            images,
            db_path: db_path.into(),
            store: Mutex::new(Store::open(db_path)?),
            sync_lock: Mutex::new(()),
            config,
            client: Mutex::new(None),
        }))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/style.css", get(style))
        .route("/app.js", get(script))
        .route("/icon.svg", get(icon_svg))
        .route("/icon-180.png", get(icon_180))
        .route("/icon-512.png", get(icon_512))
        .route("/manifest.webmanifest", get(manifest))
        .route("/img", get(image))
        .route("/api/summary", get(summary))
        .route("/api/torrents", get(torrents))
        .route("/api/marks", post(set_marks))
        .route("/api/ratings", post(set_ratings))
        .route("/api/visit", post(visit))
        .route("/api/prefs", get(get_prefs).put(put_prefs))
        .route("/api/sync", get(sync_status).post(run_sync))
        .route("/api/older", post(load_older))
        .route("/api/top", post(refresh_top))
        .route("/api/torrents/{id}/details", get(details))
        .route("/api/torrents/{id}/download", get(download))
        .route("/api/torrents/{id}/refresh", post(refresh))
        .route("/api/torrents/{id}/history", get(history))
        .route("/api/activity", get(activity))
        .with_state(state)
}

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<tracker::Error> for ApiError {
    fn from(e: tracker::Error) -> Self {
        Self(StatusCode::BAD_GATEWAY, format!("tracker: {e}"))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

// The UI is embedded so the binary runs on its own. `no-cache` makes the browser
// revalidate every load, so a rebuilt server is picked up on a plain refresh.
const NO_CACHE: (header::HeaderName, &str) = (header::CACHE_CONTROL, "no-cache");

async fn index() -> impl IntoResponse {
    ([NO_CACHE], Html(include_str!("../ui/index.html")))
}

async fn style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8"), NO_CACHE],
        include_str!("../ui/style.css"),
    )
}

async fn script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            NO_CACHE,
        ],
        include_str!("../ui/app.js"),
    )
}

/// Icons and the manifest let a phone add bbt to its home screen as an app.
const ICON_CACHE: (header::HeaderName, &str) = (header::CACHE_CONTROL, "max-age=86400");

async fn icon_svg() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml"), ICON_CACHE],
        include_str!("../ui/icon.svg"),
    )
}

async fn icon_180() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/png"), ICON_CACHE],
        include_bytes!("../ui/icon-180.png").as_slice(),
    )
}

async fn icon_512() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/png"), ICON_CACHE],
        include_bytes!("../ui/icon-512.png").as_slice(),
    )
}

async fn manifest() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            NO_CACHE,
        ],
        include_str!("../ui/manifest.webmanifest"),
    )
}

#[derive(Deserialize)]
struct SummaryQuery {
    from: Option<i64>,
}

async fn summary(
    State(s): State<Arc<AppState>>,
    Query(q): Query<SummaryQuery>,
) -> ApiResult<Value> {
    let store = s.store.lock().await;
    let sum = store.summary(q.from)?;
    Ok(Json(json!({
        "counts": sum,
        "last_sync": store.last_sync()?,
        "last_visit": store.get("last_visit")?.and_then(|v| v.parse::<i64>().ok()),
        "coverage_from": coverage_from(&store)?,
        "top": {
            "count": store.count_top_seeded()?,
            "fetched_at": store.get(top::FETCHED_KEY)?.and_then(|v| v.parse::<i64>().ok()),
        },
        "categories": s.config.as_ref().map_or(Vec::new(), |c| {
            c.site
                .categories
                .iter()
                .map(|(id, name)| json!({"id": id, "name": name}))
                .collect()
        }),
        // `{id}` and `{hash_id}` are filled in by the UI.
        "details_url": s.config.as_ref().map(|c| format!(
            "{}/{}",
            c.base_url.trim_end_matches('/'),
            c.site.details_template()
        )),
    })))
}

#[derive(Deserialize, Default)]
struct TorrentsQuery {
    view: Option<String>,
    from: Option<i64>,
    /// Comma-separated category ids.
    cats: Option<String>,
    q: Option<String>,
    free: Option<bool>,
    sticky: Option<bool>,
    sort: Option<String>,
    limit: Option<u32>,
}

impl TorrentsQuery {
    fn into_filter(self) -> Result<Filter, ApiError> {
        let view = match self.view.as_deref() {
            None => View::New,
            Some(v) => View::parse(v)
                .ok_or_else(|| ApiError::bad_request(format!("unknown view {v:?}")))?,
        };
        let sort = match self.sort.as_deref() {
            None => Sort::Newest,
            Some(v) => Sort::parse(v)
                .ok_or_else(|| ApiError::bad_request(format!("unknown sort {v:?}")))?,
        };
        let categories = self
            .cats
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|c| !c.trim().is_empty())
            .map(|c| {
                c.trim()
                    .parse::<u16>()
                    .map_err(|_| ApiError::bad_request(format!("bad category {c:?}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(Filter {
            view,
            from: self.from,
            categories,
            search: self.q,
            free_only: self.free.unwrap_or(false),
            include_sticky: self.sticky.unwrap_or(false),
            sort,
            limit: self.limit.unwrap_or(0),
        })
    }
}

async fn torrents(
    State(s): State<Arc<AppState>>,
    Query(q): Query<TorrentsQuery>,
) -> ApiResult<Value> {
    let filter = q.into_filter()?;
    let rows = s.store.lock().await.list_torrents(&filter)?;
    Ok(Json(json!({ "torrents": rows })))
}

#[derive(Deserialize)]
struct MarksBody {
    ids: Vec<u64>,
    /// `null` moves the torrents back to New.
    mark: Option<Mark>,
}

#[derive(Serialize)]
struct Previous {
    id: u64,
    mark: Option<Mark>,
}

async fn set_marks(
    State(s): State<Arc<AppState>>,
    Json(body): Json<MarksBody>,
) -> ApiResult<Value> {
    if body.ids.is_empty() {
        return Err(ApiError::bad_request("ids is empty"));
    }
    let now = chrono::Utc::now().timestamp();
    let previous: Vec<Previous> = s
        .store
        .lock()
        .await
        .set_marks(&body.ids, body.mark, now)?
        .into_iter()
        .map(|(id, mark)| Previous { id, mark })
        .collect();
    Ok(Json(json!({ "previous": previous })))
}

/// Every mark change of one torrent, newest first (`mark: null` is back to New).
async fn history(State(s): State<Arc<AppState>>, Path(id): Path<u64>) -> ApiResult<Value> {
    let changes: Vec<Value> = s
        .store
        .lock()
        .await
        .mark_history(id)?
        .into_iter()
        .map(|(mark, at)| json!({ "mark": mark, "at": at }))
        .collect();
    Ok(Json(json!({ "changes": changes })))
}

#[derive(Deserialize)]
struct ActivityQuery {
    /// The browser's time zone, in seconds east of UTC.
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_activity_days")]
    days: i64,
}

fn default_activity_days() -> i64 {
    84
}

/// Torrents sorted per day over the last `days` days, counted in the browser's zone.
/// `today` is the current day number, so the UI can lay out empty days too.
async fn activity(
    State(s): State<Arc<AppState>>,
    Query(q): Query<ActivityQuery>,
) -> ApiResult<Value> {
    if !(-14 * 3600..=14 * 3600).contains(&q.offset) {
        return Err(ApiError::bad_request(
            "offset must be within 14 hours of UTC",
        ));
    }
    let days = q.days.clamp(1, 371);
    let today = (chrono::Utc::now().timestamp() + q.offset).div_euclid(86_400);
    let since = (today - days + 1) * 86_400 - q.offset;
    let counts: Vec<Value> = s
        .store
        .lock()
        .await
        .sorted_per_day(since, q.offset)?
        .into_iter()
        .map(|(day, n)| json!({ "day": day, "n": n }))
        .collect();
    Ok(Json(json!({ "today": today, "days": counts })))
}

/// UI preferences kept in the database so they survive browser storage being cleared.
#[derive(Serialize, Deserialize, Default)]
struct Prefs {
    /// Selected category ids; `None` means never chosen (show all).
    categories: Option<Vec<u16>>,
}

async fn get_prefs(State(s): State<Arc<AppState>>) -> ApiResult<Prefs> {
    let raw = s.store.lock().await.get("prefs")?;
    Ok(Json(
        raw.and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default(),
    ))
}

async fn put_prefs(State(s): State<Arc<AppState>>, Json(p): Json<Prefs>) -> ApiResult<Prefs> {
    let json = serde_json::to_string(&p).map_err(|e| anyhow::anyhow!(e))?;
    s.store.lock().await.set("prefs", &json)?;
    Ok(Json(p))
}

#[derive(Deserialize)]
struct RatingsBody {
    ids: Vec<u64>,
    /// 1–5, or `null` to clear.
    stars: Option<u8>,
}

async fn set_ratings(
    State(s): State<Arc<AppState>>,
    Json(body): Json<RatingsBody>,
) -> ApiResult<Value> {
    if body.ids.is_empty() {
        return Err(ApiError::bad_request("ids is empty"));
    }
    if let Some(n) = body.stars
        && !(1..=5).contains(&n)
    {
        return Err(ApiError::bad_request(format!(
            "stars must be 1 to 5, got {n}"
        )));
    }
    let now = chrono::Utc::now().timestamp();
    let previous: Vec<Value> = s
        .store
        .lock()
        .await
        .set_ratings(&body.ids, body.stars, now)?
        .into_iter()
        .map(|(id, stars)| json!({ "id": id, "stars": stars }))
        .collect();
    Ok(Json(json!({ "previous": previous })))
}

/// Record a visit and return the previous one, for "since last visit".
async fn visit(State(s): State<Arc<AppState>>) -> ApiResult<Value> {
    let store = s.store.lock().await;
    let previous = store.get("last_visit")?.and_then(|v| v.parse::<i64>().ok());
    store.set("last_visit", &chrono::Utc::now().timestamp().to_string())?;
    Ok(Json(json!({ "previous": previous })))
}

async fn sync_status(State(s): State<Arc<AppState>>) -> ApiResult<Value> {
    let store = s.store.lock().await;
    let now = chrono::Utc::now().timestamp();
    Ok(Json(json!({
        "running": s.sync_lock.try_lock().is_err(),
        "last_sync": store.last_sync()?,
        "cooldown_seconds": sync::cooldown_left(store.last_sync_attempt()?, now).unwrap_or(0),
        "max_pages": HARD_MAX_PAGES,
    })))
}

async fn run_sync(State(s): State<Arc<AppState>>) -> ApiResult<Value> {
    let Some(cfg) = s.config.as_ref() else {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "syncing is disabled".into(),
        ));
    };
    let Ok(_guard) = s.sync_lock.try_lock() else {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "a sync is already running".into(),
        ));
    };
    // Separate connection so the UI stays responsive while sync waits between pages.
    let mut store = Store::open(&s.db_path)?;
    let before = {
        let mut client = s.client.lock().await;
        if client.is_none() {
            *client = Some(config::client(&store, cfg)?);
        }
        client.as_ref().expect("client was just set").requests()
    };
    let mut source = SharedTracker {
        client: &s.client,
        cfg,
    };
    let now = chrono::Utc::now().timestamp();
    let outcome = sync::run(&mut store, &mut source, HARD_MAX_PAGES, now).await;
    let requests = {
        let client = s.client.lock().await;
        let client = client.as_ref().expect("client was just set");
        store.set("cookies", &client.cookie_header())?;
        client.requests() - before // includes any details read between pages
    };
    Ok(Json(match outcome? {
        Outcome::CoolingDown { seconds_left } => json!({
            "status": "cooling_down",
            "seconds_left": seconds_left,
        }),
        Outcome::Done(r) => json!({
            "status": "done",
            "pages": r.pages,
            "requests": requests,
            "seen": r.seen,
            "new_count": r.new_count,
            "stop": r.stop.describe(),
            "complete": r.stop == sync::Stop::ReachedCutoff,
        }),
    }))
}

/// List pages read through the shared client, locked for one page at a time rather
/// than for the whole sync: a download or details read waits for a page, not for the
/// sync. The client's request gap still spaces every tracker request the same.
struct SharedTracker<'a> {
    client: &'a Mutex<Option<Client>>,
    cfg: &'a Config,
}

impl sync::PageSource for SharedTracker<'_> {
    async fn fetch_page(&mut self, n: u32) -> tracker::Result<bbt_domain::ListPage> {
        let mut client = self.client.lock().await;
        let client = client.as_mut().expect("client created before the sync");
        let html = client
            .fetch_html(&self.cfg.site.list_page(n), &self.cfg.creds)
            .await?;
        Ok(parse_list_page(&html))
    }
}

/// Read the tracker's most seeded list again (a few pages), unless it was read within
/// the cooldown. Either way the UI then lists `view=top`.
async fn refresh_top(State(s): State<Arc<AppState>>) -> ApiResult<Value> {
    let Some(cfg) = s.config.as_ref() else {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "tracker access is disabled".into(),
        ));
    };
    let Ok(_guard) = s.sync_lock.try_lock() else {
        return Err(ApiError(StatusCode::CONFLICT, "a sync is running".into()));
    };
    let mut store = Store::open(&s.db_path)?;
    let mut client = s.client.lock().await;
    if client.is_none() {
        *client = Some(config::client(&store, cfg)?);
    }
    let client = client.as_mut().expect("client was just set");
    let now = chrono::Utc::now().timestamp();
    let mut source = top::Tracker {
        client: &mut *client,
        creds: &cfg.creds,
        site: &cfg.site,
    };
    let outcome = top::refresh(&mut store, &mut source, now).await;
    store.set("cookies", &client.cookie_header())?;
    Ok(Json(match outcome? {
        top::Outcome::CoolingDown { seconds_left } => json!({
            "status": "cooling_down",
            "seconds_left": seconds_left,
        }),
        top::Outcome::Done { pages, count } => json!({
            "status": "done",
            "pages": pages,
            "count": count,
        }),
    }))
}

/// Oldest upload time reached without gaps: the stored older-page boundary, or the
/// oldest synced upload before any older page was loaded.
fn coverage_from(store: &Store) -> anyhow::Result<Option<i64>> {
    match store.get("older_frontier")?.and_then(|v| v.parse().ok()) {
        Some(t) => Ok(Some(t)),
        None => Ok(store.summary(None)?.oldest_uploaded_at),
    }
}

/// Load exactly one older tracker list page (the user scrolled to the end).
async fn load_older(State(s): State<Arc<AppState>>) -> ApiResult<Value> {
    let Some(cfg) = s.config.as_ref() else {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "tracker access is disabled".into(),
        ));
    };
    let Ok(_guard) = s.sync_lock.try_lock() else {
        return Err(ApiError(StatusCode::CONFLICT, "a sync is running".into()));
    };
    let now = chrono::Utc::now().timestamp();
    let today = (chrono::Utc::now() + chrono::Duration::hours(7))
        .format("%Y-%m-%d")
        .to_string();

    let (frontier, cursor) = {
        let store = s.store.lock().await;
        let left = older::budget_left(store.get("older_budget")?.as_deref(), &today);
        if left == 0 {
            return Ok(Json(json!({ "status": "budget", "budget_left": 0 })));
        }
        let Some(frontier) = coverage_from(&store)? else {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "sync once before loading older uploads".into(),
            ));
        };
        let cursor = match store.get("older_cursor")?.and_then(|c| c.parse().ok()) {
            Some(page) => Cursor {
                page,
                linked: store.get("older_linked")?.as_deref() == Some("1"),
            },
            None => {
                let known = store.count_uploaded_since(frontier)?;
                let last = store.last_sync()?.map_or(now, |r| r.started_at);
                let rate = known as f64 / (last - frontier).max(3600) as f64;
                Cursor {
                    page: older::initial_page(known, rate, now - last),
                    linked: false,
                }
            }
        };
        (frontier, cursor)
    };

    let mut client = s.client.lock().await;
    if client.is_none() {
        *client = Some(config::client(&*s.store.lock().await, cfg)?);
    }
    let client = client.as_mut().expect("client was just set");
    let result = client
        .fetch_html(&cfg.site.list_page(cursor.page), &cfg.creds)
        .await;

    let mut store = s.store.lock().await;
    store.set("cookies", &client.cookie_header())?;
    let budget = older::budget_after_use(store.get("older_budget")?.as_deref(), &today);
    store.set("older_budget", &budget)?;
    let page = parse_list_page(&result?);
    if page.skipped > page.torrents.len() {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "tracker page layout not recognised".into(),
        ));
    }
    let Some(step) = older::step(cursor, &page, frontier) else {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            format!("tracker page {} was empty", cursor.page),
        ));
    };
    let added = store.upsert_torrents(&page.torrents, now)?;
    store.set("older_frontier", &step.frontier.to_string())?;
    store.set("older_cursor", &step.next.page.to_string())?;
    store.set("older_linked", if step.next.linked { "1" } else { "0" })?;
    Ok(Json(json!({
        "status": "done",
        "page": cursor.page,
        "added": added,
        "outcome": step.outcome.as_str(),
        "frontier": step.frontier,
        "budget_left": older::budget_left(Some(&budget), &today),
    })))
}

/// Fetch a torrent's details page through the shared client (respects the request gap).
/// Read the torrent's details page from the tracker (one request) and save the
/// live counts it shows. Screenshot links are left unresolved: see `fetch_details`.
async fn read_details_page(s: &AppState, id: u64) -> Result<Details, ApiError> {
    let Some(cfg) = s.config.as_ref() else {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "tracker access is disabled".into(),
        ));
    };
    let hash_id = {
        let store = s.store.lock().await;
        store
            .torrent_ref(id)?
            .ok_or_else(|| {
                ApiError(
                    StatusCode::NOT_FOUND,
                    format!("torrent {id} is not in the database"),
                )
            })?
            .0
    };
    let html = {
        let mut guard = s.client.lock().await;
        if guard.is_none() {
            *guard = Some(config::client(&*s.store.lock().await, cfg)?);
        }
        let client = guard.as_mut().expect("client was just set");
        let result = client
            .fetch_html(&cfg.site.details_page(id, hash_id), &cfg.creds)
            .await;
        s.store
            .lock()
            .await
            .set("cookies", &client.cookie_header())?;
        result
    }?; // the tracker is free again
    let details = parse_details_page(&html);
    if let Some(c) = &details.counts {
        let now = chrono::Utc::now().timestamp();
        // the page is fresher than the last list sync
        s.store.lock().await.save_counts(id, c, now)?;
    }
    Ok(details)
}

/// The details page plus the images behind its screenshot links (image hosts, not the
/// tracker), saved so the next look is served from the cache.
async fn fetch_details(s: &AppState, id: u64) -> Result<Details, ApiError> {
    let mut details = read_details_page(s, id).await?;
    let base = &s
        .config
        .as_ref()
        .expect("checked by read_details_page")
        .base_url;
    for image in hosts::resolve(&details.screenshot_links, base).await {
        if !details.images.contains(&image) {
            details.images.push(image);
        }
    }
    let now = chrono::Utc::now().timestamp();
    s.store.lock().await.save_images(id, &details.images, now)?;
    s.images.warm(details.images.clone()); // ready before they are shown
    Ok(details)
}

#[derive(Deserialize)]
struct ImageQuery {
    u: String,
}

/// A poster or screenshot from the local copy, read from its host the first time.
/// The browser keeps it for good. If this machine can't get it, the browser is sent
/// to the host instead, as before the copies existed.
async fn image(State(s): State<Arc<AppState>>, Query(q): Query<ImageQuery>) -> Response {
    if !(q.u.starts_with("https://") || q.u.starts_with("http://")) {
        return ApiError::bad_request("not a web address").into_response();
    }
    match s.images.get(&q.u).await {
        Some(img) => (
            [
                (header::CONTENT_TYPE, img.kind),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            img.bytes,
        )
            .into_response(),
        None => (
            StatusCode::TEMPORARY_REDIRECT,
            [
                (header::LOCATION, q.u.as_str()),
                (header::CACHE_CONTROL, "no-store"),
            ],
        )
            .into_response(),
    }
}

/// Counts just read from a details page, in the shape the list uses.
fn counts_json(d: &Details) -> Value {
    d.counts.map_or(Value::Null, |c| {
        json!({
            "seeders": c.seeders,
            "leechers": c.leechers,
            "snatched": c.snatched,
            "stats_at": chrono::Utc::now().timestamp(),
        })
    })
}

#[derive(Deserialize)]
struct DetailsQuery {
    /// Answer from the cache only: `images` is null when nothing is saved yet.
    #[serde(default)]
    saved: bool,
}

/// Images from the details page, fetched once and then served from the cache.
/// A fresh fetch also brings the torrent's live counts.
async fn details(
    State(s): State<Arc<AppState>>,
    Path(id): Path<u64>,
    Query(q): Query<DetailsQuery>,
) -> ApiResult<Value> {
    if let Some(images) = s.store.lock().await.cached_images(id)? {
        return Ok(Json(json!({ "images": images, "cached": true })));
    }
    if q.saved {
        return Ok(Json(json!({ "images": null, "cached": false })));
    }
    let d = fetch_details(&s, id).await?;
    Ok(Json(
        json!({ "images": d.images, "cached": false, "counts": counts_json(&d) }),
    ))
}

/// Counts read under this many seconds ago are not read again.
const REFRESH_COOLDOWN_SECS: i64 = 60;

/// Read the details page again for live counts (one tracker request, on request only).
async fn refresh(State(s): State<Arc<AppState>>, Path(id): Path<u64>) -> ApiResult<Value> {
    let stats_at = s.store.lock().await.stats_at(id)?.ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            format!("torrent {id} is not in the database"),
        )
    })?;
    if chrono::Utc::now().timestamp() - stats_at < REFRESH_COOLDOWN_SECS {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Counts were updated under a minute ago.".into(),
        ));
    }
    // Only the counts are wanted: the image hosts behind the screenshots are not asked.
    let d = read_details_page(&s, id).await?;
    Ok(Json(json!({ "counts": counts_json(&d) })))
}

/// Fetch a fresh signed link from the details page, then pass the .torrent through.
async fn download(
    State(s): State<Arc<AppState>>,
    Path(id): Path<u64>,
) -> Result<Response, ApiError> {
    // Only the signed link is wanted: the image hosts behind the screenshots are not asked.
    let d = read_details_page(&s, id).await?;
    let path = d.download_path.ok_or_else(|| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "no download link on the details page".into(),
        )
    })?;
    let title = s
        .store
        .lock()
        .await
        .torrent_ref(id)?
        .map(|(_, t)| t)
        .unwrap_or_default();
    let file = s
        .client
        .lock()
        .await
        .as_mut()
        .expect("client created by read_details_page")
        .fetch_file(&path)
        .await?;
    let disposition = format!(
        "attachment; filename=\"tracker-{id}.torrent\"; filename*=UTF-8''{}.torrent",
        percent_encode(&title)
    );
    let mut resp = file.bytes.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-bittorrent"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(|e| anyhow::anyhow!(e))?,
    );
    Ok(resp)
}

/// RFC 5987 encoding for `filename*`.
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use bbt_domain::ListedTorrent;
    use chrono::{TimeZone, Utc};
    use http_body_util::BodyExt;
    use sha2::Digest;
    use tower::ServiceExt;

    fn torrent(id: u64, at: i64) -> ListedTorrent {
        ListedTorrent {
            id,
            hash_id: id,
            category: 3,
            title: format!("Sample {id}"),
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

    async fn app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let state = AppState::new(path.to_str().unwrap(), None).unwrap();
        state
            .store
            .lock()
            .await
            .upsert_torrents(&[torrent(1, 100), torrent(2, 200)], 300)
            .unwrap();
        (router(state), dir)
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn ids(v: &Value) -> Vec<u64> {
        v["torrents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_u64().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn lists_marks_and_undoes() {
        let (app, _dir) = app().await;
        let (st, v) = call(&app, "GET", "/api/torrents", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(ids(&v), [2, 1]);

        let (st, v) = call(
            &app,
            "POST",
            "/api/marks",
            Some(json!({"ids": [2], "mark": "view_later"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["previous"], json!([{"id": 2, "mark": null}]));

        let (_, v) = call(&app, "GET", "/api/torrents?view=view_later", None).await;
        assert_eq!(ids(&v), [2]);
        assert_eq!(v["torrents"][0]["mark"], "view_later");

        let (_, v) = call(&app, "GET", "/api/summary", None).await;
        assert_eq!(v["counts"]["new"], 1);
        assert_eq!(v["counts"]["view_later"], 1);

        call(
            &app,
            "POST",
            "/api/marks",
            Some(json!({"ids": [2], "mark": null})),
        )
        .await;
        let (_, v) = call(&app, "GET", "/api/torrents?from=150", None).await;
        assert_eq!(ids(&v), [2]);
    }

    #[tokio::test]
    async fn refresh_needs_a_known_torrent_and_waits_a_minute() {
        let (app, dir) = app().await;
        let (st, _) = call(&app, "POST", "/api/torrents/9/refresh", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        // Counts from long ago: it would read the tracker, which tests have turned off.
        let (st, _) = call(&app, "POST", "/api/torrents/1/refresh", None).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let c = bbt_domain::Counts {
            seeders: 4,
            leechers: 2,
            snatched: 7,
        };
        store
            .save_counts(1, &c, chrono::Utc::now().timestamp())
            .unwrap();
        let (st, v) = call(&app, "POST", "/api/torrents/1/refresh", None).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(v["error"], "Counts were updated under a minute ago.");
        let (_, v) = call(&app, "GET", "/api/torrents?view=all", None).await;
        let t = &v["torrents"].as_array().unwrap()[1];
        assert_eq!(
            (t["seeders"].clone(), t["snatched"].clone()),
            (json!(4), json!(7))
        );
    }

    #[tokio::test]
    async fn keeps_mark_history() {
        let (app, _dir) = app().await;
        for mark in [json!("viewed"), json!(null), json!("viewed")] {
            call(
                &app,
                "POST",
                "/api/marks",
                Some(json!({"ids": [1], "mark": mark})),
            )
            .await;
        }
        let (st, v) = call(&app, "GET", "/api/torrents/1/history", None).await;
        assert_eq!(st, StatusCode::OK);
        let marks: Vec<&Value> = v["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| &c["mark"])
            .collect();
        assert_eq!(marks, [&json!("viewed"), &json!(null), &json!("viewed")]);
    }

    #[tokio::test]
    async fn counts_todays_sorting() {
        let (app, _dir) = app().await;
        call(
            &app,
            "POST",
            "/api/marks",
            Some(json!({"ids": [1, 2], "mark": "viewed"})),
        )
        .await;
        let (st, v) = call(&app, "GET", "/api/activity?offset=25200", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["days"], json!([{"day": v["today"], "n": 2}]));
        let (st, _) = call(&app, "GET", "/api/activity?offset=90000", None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn serves_the_home_screen_icon_and_manifest() {
        let (app, _dir) = app().await;
        let (st, v) = call(&app, "GET", "/manifest.webmanifest", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["display"], "standalone");
        for icon in v["icons"].as_array().unwrap() {
            let res = app
                .clone()
                .oneshot(
                    Request::get(icon["src"].as_str().unwrap())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(
                res.headers()[header::CONTENT_TYPE],
                icon["type"].as_str().unwrap()
            );
        }
    }

    #[tokio::test]
    async fn rejects_bad_input() {
        let (app, _dir) = app().await;
        let (st, v) = call(&app, "GET", "/api/torrents?view=bogus", None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "unknown view \"bogus\"");
        let (st, _) = call(&app, "GET", "/api/torrents?cats=3,x", None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _) = call(
            &app,
            "POST",
            "/api/marks",
            Some(json!({"ids": [1], "mark": "eaten"})),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        let (st, _) = call(
            &app,
            "POST",
            "/api/marks",
            Some(json!({"ids": [], "mark": null})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rates_and_validates() {
        let (app, _dir) = app().await;
        let (st, v) = call(
            &app,
            "POST",
            "/api/ratings",
            Some(json!({"ids": [1], "stars": 5})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["previous"], json!([{"id": 1, "stars": null}]));
        let (_, v) = call(&app, "GET", "/api/torrents?sort=rating", None).await;
        assert_eq!(v["torrents"][0]["id"], 1);
        assert_eq!(v["torrents"][0]["rating"], 5);
        let (st, _) = call(
            &app,
            "POST",
            "/api/ratings",
            Some(json!({"ids": [1], "stars": 6})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _) = call(
            &app,
            "POST",
            "/api/ratings",
            Some(json!({"ids": [1], "stars": null})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn saves_prefs() {
        let (app, _dir) = app().await;
        let (_, v) = call(&app, "GET", "/api/prefs", None).await;
        assert_eq!(v, json!({"categories": null}));
        let body = json!({"categories": [3, 5]});
        let (st, _) = call(&app, "PUT", "/api/prefs", Some(body.clone())).await;
        assert_eq!(st, StatusCode::OK);
        let (_, v) = call(&app, "GET", "/api/prefs", None).await;
        assert_eq!(v, body);
    }

    #[tokio::test]
    async fn visit_returns_previous_time() {
        let (app, _dir) = app().await;
        let (_, v) = call(&app, "POST", "/api/visit", None).await;
        assert_eq!(v["previous"], Value::Null);
        let (_, v) = call(&app, "POST", "/api/visit", None).await;
        assert!(v["previous"].as_i64().unwrap() > 1_700_000_000);
    }

    #[tokio::test]
    async fn serves_cached_details_without_touching_tracker() {
        let (app, dir) = app().await;
        let store = Store::open(dir.path().join("t.db")).unwrap();
        store
            .save_images(1, &["https://a/1.jpg".into()], 5)
            .unwrap();
        let (st, v) = call(&app, "GET", "/api/torrents/1/details", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v, json!({"images": ["https://a/1.jpg"], "cached": true}));

        let (st, v) = call(&app, "GET", "/api/torrents/1/details?saved=true", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["images"], json!(["https://a/1.jpg"]));
        let (st, v) = call(&app, "GET", "/api/torrents/2/details?saved=true", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v, json!({"images": null, "cached": false}));

        let (st, _) = call(&app, "GET", "/api/torrents/2/details", None).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
        let (st, _) = call(&app, "GET", "/api/torrents/2/download", None).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn serves_saved_images_for_the_browser_to_keep() {
        let (app, dir) = app().await;
        let url = "https://images.example/p.png";
        let name: String = sha2::Sha256::digest(url.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(dir.path().join("images").join(name), b"\x89PNG\r\n").unwrap();
        let req = Request::get(format!("/img?u={}", percent_encode(url)))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/png");
        assert!(
            resp.headers()[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("immutable")
        );

        let (st, _) = call(&app, "GET", "/img?u=file%3A%2F%2F%2Fetc%2Fpasswd", None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        // Not something this machine may fetch: the browser is sent to the host itself.
        let req = Request::get("/img?u=http%3A%2F%2F127.0.0.1%2Fa.png")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(resp.headers()[header::LOCATION], "http://127.0.0.1/a.png");
    }

    #[tokio::test]
    async fn serves_the_ui() {
        let (app, _dir) = app().await;
        for path in ["/", "/style.css", "/app.js"] {
            let req = Request::builder().uri(path).body(Body::empty()).unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-cache", "{path}");
        }
    }

    #[test]
    fn encodes_filenames() {
        assert_eq!(percent_encode("A b/ไ"), "A%20b%2F%E0%B9%84");
    }

    #[tokio::test]
    async fn older_disabled_without_config() {
        let (app, _dir) = app().await;
        let (st, _) = call(&app, "POST", "/api/older", None).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn top_disabled_without_config() {
        let (app, _dir) = app().await;
        let (status, _) = call(&app, "POST", "/api/top", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let (status, body) = call(&app, "GET", "/api/torrents?view=top", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["torrents"], json!([]));
    }

    #[tokio::test]
    async fn sync_disabled_without_config() {
        let (app, _dir) = app().await;
        let (st, _) = call(&app, "POST", "/api/sync", None).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
        let (st, v) = call(&app, "GET", "/api/sync", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["running"], false);
        assert_eq!(v["max_pages"], 12);
    }
}
