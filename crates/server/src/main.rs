use anyhow::{Context, Result, bail};
use bbt_server::store::Store;
use bbt_server::sync::{self, HARD_MAX_PAGES, Outcome, Tracker};
use bbt_server::tracker::{self, Client, Credentials};
use std::time::Duration;

const USAGE: &str = "usage: bbt-server sync [--max-pages N]   (N is 1 to 12, default 12)";
/// Minimum gap between any two requests to tracker.
const REQUEST_GAP: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sync") => sync_cmd(&args[1..]).await,
        _ => bail!(USAGE),
    }
}

async fn sync_cmd(args: &[String]) -> Result<()> {
    let max_pages = match args {
        [] => HARD_MAX_PAGES,
        [flag, n] if flag == "--max-pages" => n.parse().context(USAGE)?,
        _ => bail!(USAGE),
    };
    if !(1..=HARD_MAX_PAGES).contains(&max_pages) {
        bail!(USAGE);
    }

    let db_path = std::env::var("BBT_DB").unwrap_or_else(|_| "bbt.db".into());
    let mut store = Store::open(&db_path).with_context(|| format!("opening {db_path}"))?;
    let creds = Credentials {
        username: env("TRACKER_USERNAME")?,
        password: env("TRACKER_PASSWORD")?,
    };
    let base = std::env::var("TRACKER_BASE_URL").unwrap_or_else(|_| tracker::BASE_URL.into());
    let device_id = device_id(&store)?;
    let cookies = store.get("cookies")?;
    let mut client = Client::new(&base, &device_id, cookies.as_deref(), REQUEST_GAP)?;

    let now = chrono::Utc::now().timestamp();
    let mut source = Tracker {
        client: &mut client,
        creds: &creds,
    };
    let outcome = sync::run(&mut store, &mut source, max_pages, now).await;
    store.set("cookies", &client.cookie_header())?;

    match outcome? {
        Outcome::CoolingDown { seconds_left } => {
            println!(
                "Synced recently. Next sync allowed in {} min.",
                (seconds_left + 59) / 60
            );
        }
        Outcome::Done(r) => {
            println!(
                "Read {} page(s), {} requests: {} torrents, {} new. Stopped: {}.",
                r.pages,
                client.requests(),
                r.seen,
                r.new_count,
                r.stop.describe()
            );
            println!("Database now holds {} torrents.", store.torrent_count()?);
        }
    }
    Ok(())
}

/// Reuse one device id forever: env `TRACKER_DEVICE_ID`, else the stored one, else a new one.
fn device_id(store: &Store) -> Result<String> {
    if let Ok(id) = std::env::var("TRACKER_DEVICE_ID") {
        store.set("device_id", &id)?;
        return Ok(id);
    }
    if let Some(id) = store.get("device_id")? {
        return Ok(id);
    }
    let id = tracker::generate_device_id();
    store.set("device_id", &id)?;
    Ok(id)
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} is missing from .env"))
}
