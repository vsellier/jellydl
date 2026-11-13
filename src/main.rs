use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use jellydl::api::{Jellyfin, client, identity};
use jellydl::config::{self, Overrides};
use jellydl::download::{self, ManagerConfig};
use jellydl::ui::app::{self, App, AppEvent};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

/// Browse a Jellyfin library and download videos from the terminal.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Config file [default: ~/.config/jellydl/config.toml]
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Proxy URL: http://, https://, socks5:// or socks5h:// (overrides the config)
    #[arg(long)]
    proxy: Option<String>,
    /// Target directory for downloads (overrides the config)
    #[arg(short, long)]
    download_dir: Option<PathBuf>,
    /// Max downloads at the same time (overrides the config)
    #[arg(short = 'j', long)]
    max_parallel: Option<usize>,
    /// Total download speed limit, e.g. 500K, 2MB (overrides the config; 0 = no limit)
    #[arg(long)]
    max_speed: Option<String>,
    /// Do not show posters (overrides the config)
    #[arg(long)]
    no_posters: bool,
    /// Log in, list the libraries, then exit. Use it to test auth and proxy.
    #[arg(long)]
    check: bool,
    /// Run the web interface instead of the terminal interface.
    #[arg(long)]
    serve: bool,
    /// Address of the web interface. There is no login: keep it local, or put
    /// a reverse proxy with authentication in front.
    #[arg(long, env = "JELLYDL_LISTEN", default_value = "127.0.0.1:8080")]
    listen: std::net::SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let dirs = config::project_dirs()?;
    let data_dir = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("cannot create {}", data_dir.display()))?;

    // Log to a file: stdout belongs to the TUI.
    let (writer, _guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::never(&data_dir, "jellydl.log"));
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("JELLYDL_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(writer)
        .with_ansi(false)
        .init();

    let cfg_path = match cli.config {
        Some(p) => p,
        None => config::default_config_path()?,
    };
    let ov = Overrides {
        proxy: cli.proxy,
        download_dir: cli.download_dir,
        max_parallel: cli.max_parallel,
        max_speed: cli.max_speed,
    };
    let cfg = config::load(&cfg_path, &ov)?;
    let proxy_label = cfg.proxy.as_deref().map(config::redact_proxy);

    let http = client::build_http(&cfg)?;
    let identity = identity::Identity::new(&data_dir, &cfg.identity);
    eprintln!(
        "Connecting to {}{}...",
        cfg.server_url,
        proxy_label
            .as_ref()
            .map(|p| format!(" via {p}"))
            .unwrap_or_default()
    );
    let session_file = data_dir.join("session.json");
    let session = cfg.reuse_session.then_some(session_file.as_path());
    if !cfg.reuse_session {
        identity::forget_session(&session_file);
    }
    let jf = Jellyfin::connect(&cfg, http.clone(), identity, session).await?;
    tracing::info!("logged in to {} as {}", cfg.server_url, jf.user_name);

    if cli.check {
        println!("Logged in as {} ({})", jf.user_name, jf.user_id);
        println!(
            "Proxy:        {}",
            proxy_label
                .as_deref()
                .unwrap_or("none (environment variables apply)")
        );
        println!("Download dir: {}", cfg.download_dir.display());
        println!("Movies:       {}", cfg.layout.movies_dir.display());
        println!("Series:       {}", cfg.layout.series_ongoing_dir.display());
        println!("Series ended: {}", cfg.layout.series_ended_dir.display());
        println!("User-Agent:   {}", cfg.user_agent);
        println!("Auth header:  {}", jf.identity.header(Some("<token>")));
        for (k, v) in &cfg.headers {
            println!("Header:       {k}: {v}");
        }
        println!(
            "Session:      {}",
            match (cfg.reuse_session, jf.reused_session) {
                (true, true) => "saved session reused (no new login)",
                (true, false) => "new login, session saved for next time",
                (false, _) => "new login (reuse_session = false)",
            }
        );
        println!(
            "Speed limit:  {}",
            cfg.max_speed
                .map(|b| format!("{}/s", jellydl::ui::fmt::bytes(b)))
                .unwrap_or_else(|| "none".into())
        );
        println!(
            "Start delay:  {}",
            cfg.start_delay
                .map(|(a, b)| format!("{a}-{b} s between downloads"))
                .unwrap_or_else(|| "none".into())
        );
        println!("Libraries:");
        for v in jf.views().await? {
            println!(
                "  - {} [{}]",
                v.name,
                v.collection_type.as_deref().unwrap_or("mixed")
            );
        }
        return Ok(());
    }

    std::fs::create_dir_all(&cfg.download_dir)
        .with_context(|| format!("cannot create {}", cfg.download_dir.display()))?;

    let (mgr_tx, mut mgr_rx) = mpsc::unbounded_channel();
    let mgr = download::spawn(
        ManagerConfig {
            client: http,
            auth_header: jf.auth_header().to_string(),
            max_parallel: cfg.max_parallel,
            queue_file: Some(data_dir.join("queue.json")),
            retry_base: Duration::from_secs(2),
            max_retries: 3,
            max_speed: cfg.max_speed,
            start_delay: cfg
                .start_delay
                .map(|(a, b)| (Duration::from_secs(a), Duration::from_secs(b))),
        },
        mgr_tx,
    );

    if cli.serve {
        let res = jellydl::web::serve(
            jf,
            mgr.clone(),
            mgr_rx,
            cfg.layout.clone(),
            cfg.max_parallel,
            cli.listen,
        )
        .await;
        eprintln!("Saving the download queue...");
        mgr.shutdown().await;
        return res;
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let fwd = tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = mgr_rx.recv().await {
            if fwd.send(AppEvent::Mgr(ev)).is_err() {
                break;
            }
        }
    });
    let ui = App::new(
        jf,
        tx,
        mgr.clone(),
        cfg.layout.clone(),
        proxy_label,
        cfg.max_parallel,
        cfg.max_speed,
    )
    .with_state_file(data_dir.join("ui_state.json"));
    let res = app::run(ui, rx, cfg.posters && !cli.no_posters).await;
    eprintln!("Saving the download queue...");
    mgr.shutdown().await;
    res
}
