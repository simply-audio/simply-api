//! `simply-relay-proxy`: see the README.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use simply_relay_proxy::config::{self, Config};
use simply_relay_proxy::{Options, Proxy};

const USAGE: &str = "\
Serve the MCP servers on this machine to Lumina, as one device.

Usage:
  simply-relay-proxy [--config FILE]            run the proxy
  simply-relay-proxy [--config FILE] pair CODE  link this machine with the
                                                code from Lumina's /devices → Add device
  simply-relay-proxy --help | --version

The config file defaults to simply-relay-proxy/config.toml in your config
directory; the README shows what goes in it.";

enum Command {
    Run,
    Pair(String),
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<(PathBuf, Command)>, String> {
    let mut config = None;
    let mut words = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("simply-relay-proxy {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-c" | "--config" => {
                config = Some(PathBuf::from(args.next().ok_or("--config needs a file")?));
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            _ => words.push(arg),
        }
    }
    let command = match words.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] | ["run"] => Command::Run,
        ["pair", code] => Command::Pair(code.to_owned()),
        ["pair"] => return Err("pair needs the code Lumina showed you".into()),
        _ => return Err(format!("unexpected arguments: {}", words.join(" "))),
    };
    let path = match config {
        Some(path) => path,
        None => config::default_dir()
            .ok_or("no config directory found; pass --config FILE")?
            .join(config::FILE_NAME),
    };
    Ok(Some((path, command)))
}

fn pair(path: &std::path::Path, code: &str) -> Result<(), String> {
    let config = Config::load(path).map_err(|e| e.to_string())?;
    let base = config::link_base(&config.relay)?;
    let request = simply_hooks::device::LinkRequest {
        code: code.to_owned(),
        app: config::APP.to_owned(),
        label: config.label.clone().unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let linked = simply_hooks::device::link(&base, &request, Duration::from_secs(20))
        .map_err(|e| format!("could not link: {e}"))?;
    let device = simply_relay::DeviceKey { device_id: linked.device_id, key: linked.key };
    config::store_device(path, &device).map_err(|e| e.to_string())?;
    println!(
        "Linked to {} on {}. The key is in {}.",
        linked.member_name,
        linked.server,
        path.display()
    );
    Ok(())
}

async fn run(path: PathBuf) -> Result<(), String> {
    let proxy = Proxy::start(&path, Options::default()).map_err(|e| e.to_string())?;
    let mut status = proxy.relay().watch_status();
    tracing::info!("proxy: config {}", path.display());
    loop {
        tokio::select! {
            changed = status.changed() => {
                if changed.is_err() { break; }
                let now = status.borrow_and_update().clone();
                tracing::info!("proxy: relay {now:?}");
                if matches!(now, simply_relay::Status::NotLinked) {
                    tracing::warn!("proxy: not linked yet; run `simply-relay-proxy pair CODE`");
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    proxy.shutdown().await;
    Ok(())
}

fn main() -> ExitCode {
    let (path, command) = match parse(std::env::args().skip(1)) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("simply-relay-proxy: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match command {
        Command::Pair(code) => pair(&path, &code),
        Command::Run => {
            use tracing_subscriber::layer::SubscriberExt;
            use tracing_subscriber::util::SubscriberInitExt;
            let filter = tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer())
                .with(simply_relay::log_layer())
                .init();
            match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime.block_on(run(path)),
                Err(e) => Err(e.to_string()),
            }
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("simply-relay-proxy: {e}");
            ExitCode::FAILURE
        }
    }
}
