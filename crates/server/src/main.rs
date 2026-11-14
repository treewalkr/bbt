use anyhow::{Context, Result, bail};
use bbt_server::config::{self, Config};
use bbt_server::store::Store;
use bbt_server::sync::{HARD_MAX_PAGES, Outcome};

const USAGE: &str = "usage: bbt-server sync [--max-pages N]   (N is 1 to 12, default 12)";

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
    let cfg = Config::from_env()?;
    let mut store =
        Store::open(&cfg.db_path).with_context(|| format!("opening {}", cfg.db_path))?;
    let live = config::sync_now(&mut store, &cfg, max_pages).await?;
    match live.outcome {
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
                live.requests,
                r.seen,
                r.new_count,
                r.stop.describe()
            );
            println!("Database now holds {} torrents.", store.torrent_count()?);
        }
    }
    Ok(())
}
