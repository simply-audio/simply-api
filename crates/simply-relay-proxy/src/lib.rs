//! **simply-relay-proxy**: the MCP servers on a person's own machine,
//! served to one or more Lumina Discord servers.
//!
//! One proxy process runs one client for each local MCP server (over
//! streamable HTTP or stdio) and opens one `simply-relay` WebSocket
//! connection per linked device (`[[devices]]`, or the legacy `[device]`
//! table). Every linked device serves the same union of local tools, named
//! `<server>.<tool>` (Lumina shows them as `device:<machine>.<server>.<tool>`),
//! behind its own device key and effective consent switches.
//!
//! - A call goes to the server that owns the tool, and its answer comes back
//!   as the server gave it.
//! - When a server starts, stops or changes its tools, every connected Lumina
//!   is told (`notifications/tools/list_changed`). A server that is down
//!   simply has no tools; the proxy tries it again with backoff, and a stdio
//!   server is started again.
//! - What Lumina may call is each device's switches ([`simply_relay::Consent`]):
//!   top-level `[consent]` is the shared default and any device may override
//!   `read_only` or individual server entries in `actions`. `read_only`
//!   covers every tool the server marks `readOnlyHint` (or the config lists
//!   as read-only), and each server's other tools are an action class named
//!   after the server.
//!
//! The config file is [`config`]'s; the binary's usage is in the README.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use simply_relay::{Backoff, Relay};

pub mod config;
pub mod local;
pub mod union;

pub use config::Config;
pub use local::Pacing;
pub use union::{Catalog, Union};

/// How the proxy paces itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Local servers.
    pub local: Pacing,
    /// Reconnecting to Lumina.
    pub relay: Backoff,
}

/// One linked device's running relay.
pub struct DeviceRelay {
    pub id: String,
    pub server: Option<String>,
    pub member: Option<String>,
    pub relay: Relay,
}

impl DeviceRelay {
    /// Server name if known, otherwise the first 8 characters of `id`.
    pub fn display_name(&self) -> &str {
        self.server.as_deref().filter(|s| !s.is_empty()).unwrap_or_else(|| {
            match self.id.char_indices().nth(8) {
                Some((idx, _)) => &self.id[..idx],
                None => &self.id,
            }
        })
    }
}

/// A running proxy.
pub struct Proxy {
    relays: Vec<DeviceRelay>,
    catalog: Arc<Catalog>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Proxy {
    /// Start the proxy for the config file at `path`: one relay per linked
    /// device (each keeping its own `lumina.toml` and call log under
    /// `devices/<id>/` beside the config file) and one client for every local
    /// server. Each device's key is read from the file before every dial; a
    /// new key Lumina hands over is written back into only that device's
    /// entry. Must be called within a tokio runtime.
    pub fn start(path: &Path, options: Options) -> std::io::Result<Proxy> {
        let config = Config::load(path)?;
        let dir = path.parent().map_or_else(|| PathBuf::from("."), Path::to_owned);
        let read_only =
            config.servers.iter().map(|(name, s)| (name.clone(), s.read_only.clone())).collect();
        let catalog = Catalog::new(read_only);
        let write_lock = Arc::new(Mutex::new(()));

        let devices: Vec<config::Device> = config.all_devices().into_iter().cloned().collect();
        if devices.is_empty() {
            tracing::warn!(
                "proxy: no linked devices; run `simply-relay-proxy pair CODE` and restart"
            );
        }

        let mut relays = Vec::with_capacity(devices.len());
        for device in devices {
            let device_dir = dir.join("devices").join(&device.id);
            let device_id = device.id.clone();
            let key_path = path.to_owned();
            let new_key_path = path.to_owned();
            let config_lock = write_lock.clone();
            let classifier = catalog.clone();

            let mut builder = Relay::builder(config::APP, env!("CARGO_PKG_VERSION"), &device_dir)
                .endpoint(config.relay.clone())
                .backoff(options.relay)
                .key(move || match Config::load(&key_path) {
                    Ok(config) => config.find_device(&device_id).map(|d| d.key()),
                    Err(e) => {
                        tracing::warn!("proxy: {e}");
                        None
                    }
                })
                .on_new_key(move |updated| {
                    let _guard = config_lock.lock().unwrap_or_else(|p| p.into_inner());
                    config::store_device(&new_key_path, updated).map_err(|e| e.to_string())
                })
                .classify(move |tool| classifier.classify(tool));
            if let Some(label) = &config.label {
                builder = builder.label(label.clone());
            }
            for server in config.servers.keys() {
                builder = builder.action(server.clone(), Vec::<String>::new());
            }
            let factory = catalog.clone();
            let relay = builder.spawn(move || Union::new(factory.clone()))?;

            let effective = device.effective_consent(&config.consent);
            relay.set_read_only(effective.read_only)?;
            for server in config.servers.keys() {
                let on = effective.actions.get(server).copied().unwrap_or(false);
                relay.set_action(server, on)?;
            }
            for class in effective.actions.keys() {
                if !config.servers.contains_key(class) {
                    tracing::warn!(
                        "proxy [{}]: consent actions: no server is named {class}",
                        device.display_name()
                    );
                }
            }

            relays.push(DeviceRelay {
                id: device.id,
                server: device.server,
                member: device.member,
                relay,
            });
        }

        let mut tasks = Vec::new();
        for (name, server) in config.servers {
            let transport = server
                .transport()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            tasks.push(tokio::spawn(local::supervise(
                name,
                transport,
                catalog.clone(),
                options.local,
            )));
        }
        Ok(Proxy { relays, catalog, tasks })
    }

    /// All running device relays.
    pub fn relays(&self) -> &[DeviceRelay] {
        &self.relays
    }

    /// One device's relay, looked up by device id.
    pub fn relay(&self, device_id: &str) -> Option<&Relay> {
        self.relays.iter().find(|r| r.id == device_id).map(|r| &r.relay)
    }

    /// The local servers that are up now.
    pub fn servers_up(&self) -> Vec<String> {
        self.catalog.servers()
    }

    /// Stop: close every relay and every local connection (stdio servers are
    /// stopped).
    pub async fn shutdown(self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            let _ = task.await;
        }
        for device in self.relays {
            device.relay.shutdown().await;
        }
    }
}
