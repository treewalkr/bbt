use anyhow::{Context, Result, bail};
use bbt_server::api::{self, AppState};
use bbt_server::config::{self, Config};
use bbt_server::store::Store;
use bbt_server::sync::{HARD_MAX_PAGES, Outcome};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const USAGE: &str = "usage:\n  bbt-server sync [--max-pages N]   (N is 1 to 12, default 12)\n  bbt-server serve [--port N] [--host IP]\n    default port 4781; default host is loopback only (127.0.0.1 and ::1).\n    --host 0.0.0.0 listens on every address: only inside a container.";

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sync") => sync_cmd(&args[1..]).await,
        Some("serve") => serve_cmd(&args[1..]).await,
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
    let mut client = config::client(&store, &cfg)?;
    let live = config::sync_now(&mut store, &cfg, &mut client, max_pages).await?;
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

async fn serve_cmd(args: &[String]) -> Result<()> {
    let (mut port, mut host): (u16, Option<IpAddr>) = (4781, None);
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().context(USAGE)?;
        match flag.as_str() {
            "--port" => port = value.parse().context(USAGE)?,
            // An IP address, never a name, so it binds exactly what was asked.
            "--host" => host = Some(value.parse().context(USAGE)?),
            _ => bail!(USAGE),
        }
    }
    let cfg = Config::from_env()?;
    let state = AppState::new(&cfg.db_path.clone(), Some(cfg))?;
    let app = api::router(state);
    if let Some(ip) = host {
        let addr = SocketAddr::from((ip, port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        println!("bbt listening on {addr}");
        axum::serve(listener, app).await?;
        return Ok(());
    }
    // Loopback addresses only, never a name: `localhost` resolves to both of these.
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(v4)
        .await
        .with_context(|| format!("binding {v4}"))?;
    println!("bbt running at http://localhost:{port}");
    match tokio::net::TcpListener::bind(v6).await {
        Ok(listener6) => {
            tokio::try_join!(
                axum::serve(listener, app.clone()).into_future(),
                axum::serve(listener6, app).into_future(),
            )?;
        }
        // IPv6 loopback can be missing; IPv4 alone still serves localhost.
        Err(_) => axum::serve(listener, app).await?,
    }
    Ok(())
}
