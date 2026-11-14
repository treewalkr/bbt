//! JSON API for the local inbox UI. Binds to localhost only (see `serve`).

use crate::config::{self, Config};
use crate::older::{self, Fit};
use crate::queries::{Filter, Mark, Sort, View};
use crate::store::Store;
use crate::sync::{self, HARD_MAX_PAGES, Outcome};
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
}

impl AppState {
    pub fn new(db_path: &str, config: Option<Config>) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
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
        .route("/api/summary", get(summary))
        .route("/api/torrents", get(torrents))
        .route("/api/marks", post(set_marks))
        .route("/api/visit", post(visit))
        .route("/api/sync", get(sync_status).post(run_sync))
        .route("/api/older", post(load_older))
        .route("/api/torrents/{id}/details", get(details))
        .route("/api/torrents/{id}/download", get(download))
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

// The UI is embedded so the binary runs on its own.
async fn index() -> Html<&'static str> {
    Html(include_str!("../ui/index.html"))
}

async fn style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../ui/style.css"),
    )
}

async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../ui/app.js"),
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
    let mut client = s.client.lock().await;
    if client.is_none() {
        *client = Some(config::client(&store, cfg)?);
    }
    let client = client.as_mut().expect("client was just set");
    let live = config::sync_now(&mut store, cfg, client, HARD_MAX_PAGES).await?;
    Ok(Json(match live.outcome {
        Outcome::CoolingDown { seconds_left } => json!({
            "status": "cooling_down",
            "seconds_left": seconds_left,
        }),
        Outcome::Done(r) => json!({
            "status": "done",
            "pages": r.pages,
            "requests": live.requests,
            "seen": r.seen,
            "new_count": r.new_count,
            "stop": r.stop.describe(),
            "complete": r.stop == sync::Stop::ReachedCutoff,
        }),
    }))
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
        let Some(frontier) = store.summary(None)?.oldest_uploaded_at else {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "sync once before loading older uploads".into(),
            ));
        };
        let cursor = match store.get("older_cursor")?.and_then(|c| c.parse().ok()) {
            Some(c) => c,
            None => {
                let known = store.count_uploaded_since(frontier)?;
                let last = store.last_sync()?.map_or(now, |r| r.started_at);
                let rate = known as f64 / (last - frontier).max(3600) as f64;
                older::initial_cursor(known, rate, now - last)
            }
        };
        (frontier, cursor)
    };

    let mut client = s.client.lock().await;
    if client.is_none() {
        *client = Some(config::client(&*s.store.lock().await, cfg)?);
    }
    let client = client.as_mut().expect("client was just set");
    let path = if cursor == 0 {
        "browse.php".to_string()
    } else {
        format!("browse.php?page={cursor}")
    };
    let result = client.fetch_html(&path, &cfg.creds).await;

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
    let Some(fit) = older::classify(&page, frontier) else {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            format!("tracker page {cursor} was empty"),
        ));
    };
    let added = store.upsert_torrents(&page.torrents, now)?;
    let next = older::next_cursor(cursor, fit, &page, frontier);
    store.set("older_cursor", &next.to_string())?;
    Ok(Json(json!({
        "status": "done",
        "page": cursor,
        "added": added,
        "fit": match fit { Fit::Overlap => "overlap", Fit::Newer => "newer", Fit::Older => "older" },
        "frontier": store.summary(None)?.oldest_uploaded_at,
        "budget_left": older::budget_left(Some(&budget), &today),
    })))
}

/// Fetch a torrent's details page through the shared client (respects the request gap).
async fn fetch_details(s: &AppState, id: u64) -> Result<Details, ApiError> {
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
    let mut client = s.client.lock().await;
    if client.is_none() {
        *client = Some(config::client(&*s.store.lock().await, cfg)?);
    }
    let client = client.as_mut().expect("client was just set");
    let result = client
        .fetch_html(
            &format!("details.php?id={id}&hash_id={hash_id}"),
            &cfg.creds,
        )
        .await;
    let store = s.store.lock().await;
    store.set("cookies", &client.cookie_header())?;
    let details = parse_details_page(&result?);
    store.save_images(id, &details.images, chrono::Utc::now().timestamp())?;
    Ok(details)
}

/// Images from the details page, fetched once and then served from the cache.
async fn details(State(s): State<Arc<AppState>>, Path(id): Path<u64>) -> ApiResult<Value> {
    if let Some(images) = s.store.lock().await.cached_images(id)? {
        return Ok(Json(json!({ "images": images, "cached": true })));
    }
    let d = fetch_details(&s, id).await?;
    Ok(Json(json!({ "images": d.images, "cached": false })))
}

/// Fetch a fresh signed link from the details page, then pass the .torrent through.
async fn download(
    State(s): State<Arc<AppState>>,
    Path(id): Path<u64>,
) -> Result<Response, ApiError> {
    let d = fetch_details(&s, id).await?;
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
        .expect("client created by fetch_details")
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

        let (st, _) = call(&app, "GET", "/api/torrents/2/details", None).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
        let (st, _) = call(&app, "GET", "/api/torrents/2/download", None).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn serves_the_ui() {
        let (app, _dir) = app().await;
        for path in ["/", "/style.css", "/app.js"] {
            let req = Request::builder().uri(path).body(Body::empty()).unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
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
