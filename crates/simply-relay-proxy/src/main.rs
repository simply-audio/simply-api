//! `simply-relay-proxy`: see the README.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use simply_relay_proxy::config::{self, Config};
use simply_relay_proxy::{Options, Proxy};

const USAGE: &str = "\
Serve the MCP servers on this machine to one or more Lumina Discord servers.

Usage:
  simply-relay-proxy [--config FILE]                      run the proxy
  simply-relay-proxy [--config FILE] pair CODE            link this machine with the
                                                          code from Lumina's /devices → Add device
  simply-relay-proxy [--config FILE] devices              list linked devices
  simply-relay-proxy [--config FILE] unpair ID_OR_SERVER  unlink and remove a device
  simply-relay-proxy --help | --version

The config file defaults to simply-relay-proxy/config.toml in your config
directory; the README shows what goes in it.";

enum Command {
    Run,
    Pair(String),
    Devices,
    Unpair(String),
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
        ["devices"] => Command::Devices,
        ["unpair", target] => Command::Unpair(target.to_owned()),
        ["unpair"] => return Err("unpair needs a device id or server name".into()),
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
    let device = simply_relay::DeviceKey { device_id: linked.device_id.clone(), key: linked.key };
    config::store_linked_device(
        path,
        &device,
        Some(linked.server.as_str()),
        Some(linked.member_name.as_str()),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "Linked to {} on {} (id {}). Saved to {}.",
        linked.member_name,
        linked.server,
        linked.device_id,
        path.display()
    );
    Ok(())
}

fn format_consent_override(consent: Option<&config::ConsentOverride>) -> String {
    let Some(c) = consent.filter(|c| !c.is_empty()) else {
        return "(default)".to_owned();
    };
    let mut parts = Vec::new();
    if let Some(ro) = c.read_only {
        parts.push(format!("read_only={ro}"));
    }
    if !c.actions.is_empty() {
        let pairs: Vec<String> = c.actions.iter().map(|(k, v)| format!("{k}={v}")).collect();
        parts.push(format!("actions={{{}}}", pairs.join(", ")));
    }
    parts.join(", ")
}

fn devices(path: &std::path::Path) -> Result<(), String> {
    let config = Config::load(path).map_err(|e| e.to_string())?;
    let all = config.all_devices();
    if all.is_empty() {
        println!("No linked devices. Run `simply-relay-proxy pair CODE` to link one.");
        return Ok(());
    }
    for device in all {
        let server = device.server.as_deref().unwrap_or("-");
        let member = device.member.as_deref().unwrap_or("-");
        let consent = format_consent_override(device.consent.as_ref());
        println!("{}  server={server:?}  member={member:?}  consent: {consent}", device.id);
    }
    Ok(())
}

fn unpair(path: &std::path::Path, target: &str) -> Result<(), String> {
    let config = Config::load(path).map_err(|e| e.to_string())?;
    let device = config::resolve_device(&config, target).map_err(|e| e.to_string())?.clone();
    if let Ok(base) = config::link_base(&config.relay) {
        let user_agent = format!("{}/{}", config::APP, env!("CARGO_PKG_VERSION"));
        let key = device.key();
        if let Err(e) = simply_hooks::device::unlink(
            &base,
            &key.device_id,
            &key.key,
            &user_agent,
            Duration::from_secs(10),
        ) {
            eprintln!("Warning: could not notify Lumina ({e}); removing locally anyway.");
        }
    }
    let removed = config::remove_device(path, &device.id).map_err(|e| e.to_string())?;
    println!("Unpaired {} ({}) from {}.", removed.display_name(), removed.id, path.display());
    Ok(())
}

async fn run(path: PathBuf) -> Result<(), String> {
    let proxy = Proxy::start(&path, Options::default()).map_err(|e| e.to_string())?;
    tracing::info!("proxy: config {}", path.display());
    let mut watchers = Vec::with_capacity(proxy.relays().len());
    for device in proxy.relays() {
        let label = device.display_name().to_owned();
        let mut status = device.relay.watch_status();
        watchers.push(tokio::spawn(async move {
            while status.changed().await.is_ok() {
                let now = status.borrow_and_update().clone();
                tracing::info!("proxy [{label}]: relay {now:?}");
                if matches!(now, simply_relay::Status::NotLinked) {
                    tracing::warn!(
                        "proxy [{label}]: not linked; run `simply-relay-proxy pair CODE`"
                    );
                }
            }
        }));
    }
    let _ = tokio::signal::ctrl_c().await;
    for watcher in watchers {
        watcher.abort();
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
        Command::Devices => devices(&path),
        Command::Unpair(target) => unpair(&path, &target),
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
