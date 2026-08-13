// Owns the HIVE process entry point, CLI overrides, logging, and graceful signal handling.
// config.rs defines settings; lib.rs owns database, TLS, router, and listener assembly.

// nexus-way-hive — the HIVE backbone server binary.
//
//   Dev (current machine):   nexus-way-hive --dev
//     → 127.0.0.1:8443, self-signed TLS, data in ~/.local/share/nexus-hive-dev
//   Prod (Linux server):     nexus-way-hive --config /var/lib/nexus-hive/hive.toml
//     → run under systemd (see deploy/nexus-way-hive.service)

use anyhow::{bail, Context, Result};
use nexus_way_hive::Config;
use std::fs;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = parse_args()?;
    let handle = axum_server::Handle::new();

    // Graceful shutdown on SIGINT/SIGTERM (systemd stop).
    let h = handle.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
        }
        tracing::info!("shutdown signal received");
        h.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });

    nexus_way_hive::serve(cfg, handle).await
}

fn parse_args() -> Result<Config> {
    let mut dev = false;
    let mut data_dir: Option<PathBuf> = None;
    let mut bind = None;
    let mut config_path: Option<PathBuf> = None;
    let mut web_dir: Option<PathBuf> = None;
    let mut apk_path: Option<PathBuf> = None;
    let mut behind_proxy = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dev" => dev = true,
            "--web-dir" => {
                web_dir = Some(PathBuf::from(args.next().context("--web-dir needs a value")?))
            }
            "--apk" => {
                apk_path = Some(PathBuf::from(args.next().context("--apk needs a value")?))
            }
            "--data-dir" => {
                data_dir = Some(PathBuf::from(args.next().context("--data-dir needs a value")?))
            }
            "--bind" => {
                bind = Some(
                    args.next()
                        .context("--bind needs a value")?
                        .parse()
                        .context("--bind must be ADDR:PORT")?,
                )
            }
            "--config" => {
                config_path = Some(PathBuf::from(args.next().context("--config needs a value")?))
            }
            "--behind-proxy" => behind_proxy = true,
            other => bail!(
                "unknown argument {other}\nusage: nexus-way-hive [--dev] [--data-dir DIR] [--bind ADDR:PORT] [--config FILE] [--web-dir DIR] [--apk FILE] [--behind-proxy]"
            ),
        }
    }

    let mut cfg = match &config_path {
        Some(p) => toml::from_str::<Config>(
            &fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
        )
        .with_context(|| format!("parsing {}", p.display()))?,
        None if dev => Config::dev_defaults(),
        None => Config::default(),
    };
    if dev {
        cfg.dev = true;
    }
    if let Some(d) = data_dir {
        cfg.data_dir = d;
    }
    if let Some(b) = bind {
        cfg.bind = b;
    }
    if web_dir.is_some() {
        cfg.web_dir = web_dir;
    }
    if apk_path.is_some() {
        cfg.apk_path = apk_path;
    }
    if behind_proxy {
        cfg.behind_proxy = true;
    }
    Ok(cfg)
}
