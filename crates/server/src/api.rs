//! JSON API for the local inbox UI. Binds to localhost only (see `serve`).

use crate::config::{self, Config};
use crate::queries::{Filter, Mark, Sort, View};
use crate::store::Store;
use crate::sync::{self, HARD_MAX_PAGES, Outcome};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct AppState {
    db_path: String,
    store: Mutex<Store>,
    /// Held for the whole duration of a sync so two can never overlap.
    sync_lock: Mutex<()>,
    /// `None` disables live syncing (tests).
    config: Option<Config>,
}

impl AppState {
    pub fn new(db_path: &str, config: Option<Config>) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            db_path: db_path.into(),
            store: Mutex::new(Store::open(db_path)?),
            sync_lock: Mutex::new(()),
            config,
        }))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/summary", get(summary))
        .route("/api/torrents", get(torrents))
        .route("/api/marks", post(set_marks))
        .route("/api/visit", post(visit))
        .route("/api/sync", get(sync_status).post(run_sync))
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

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

async fn index() -> Html<&'static str> {
    Html("<!doctype html><title>bbt</title><p>bbt API is running. The inbox UI comes next.</p>")
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
    let live = config::sync_now(&mut store, cfg, HARD_MAX_PAGES).await?;
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
