//! **simply-relay-proxy**: the MCP servers on a person's own machine,
//! served to Lumina as one device.
//!
//! The proxy dials Lumina once, with the signed hello `simply-relay` says,
//! and serves one MCP server over that connection whose tools are the
//! tools of every local server that is up, named `<server>.<tool>` (Lumina
//! shows them as `device:<machine>.<server>.<tool>`). It is a plain MCP
//! client of each local server, over streamable HTTP or stdio, so no app
//! needs to know about it.
//!
//! - A call goes to the server that owns the tool, and its answer comes back
//!   as the server gave it.
//! - When a server starts, stops or changes its tools, Lumina is told
//!   (`notifications/tools/list_changed`). A server that is down simply has
//!   no tools; the proxy tries it again with backoff, and a stdio server is
//!   started again.
//! - What Lumina may call is the relay's switches ([`simply_relay::Consent`]):
//!   `read_only` covers every tool the server marks `readOnlyHint` (or the
//!   config lists as read-only), and each server's other tools are an
//!   action class named after the server.
//!
//! The config file is [`config`]'s; the binary's usage is in the README.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// A running proxy.
pub struct Proxy {
    relay: Relay,
    catalog: Arc<Catalog>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Proxy {
    /// Start the proxy for the config file at `path`: the relay (switches
    /// and call log beside the file) and a client for every local server.
    /// The device key is read from the file before every dial, so a `pair`
    /// while it runs is picked up by the next dial; a new key Lumina hands
    /// over is written back into it. Must be called within a tokio runtime.
    pub fn start(path: &Path, options: Options) -> std::io::Result<Proxy> {
        let config = Config::load(path)?;
        let dir = path.parent().map_or_else(|| PathBuf::from("."), Path::to_owned);
        let read_only =
            config.servers.iter().map(|(name, s)| (name.clone(), s.read_only.clone())).collect();
        let catalog = Catalog::new(read_only);

        let (key_path, new_key_path) = (path.to_owned(), path.to_owned());
        let classifier = catalog.clone();
        let mut builder = Relay::builder(config::APP, env!("CARGO_PKG_VERSION"), &dir)
            .endpoint(config.relay.clone())
            .backoff(options.relay)
            .key(move || match Config::load(&key_path) {
                Ok(config) => config.device.map(|d| d.key()),
                Err(e) => {
                    tracing::warn!("proxy: {e}");
                    None
                }
            })
            .on_new_key(move |device| {
                config::store_device(&new_key_path, device).map_err(|e| e.to_string())
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

        relay.set_read_only(config.consent.read_only)?;
        for (class, on) in &config.consent.actions {
            if let Err(e) = relay.set_action(class, *on) {
                tracing::warn!("proxy: [consent] actions: {e}; no server is named {class}");
            }
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
        Ok(Proxy { relay, catalog, tasks })
    }

    /// The relay, for its status and call log.
    pub fn relay(&self) -> &Relay {
        &self.relay
    }

    /// The local servers that are up now.
    pub fn servers_up(&self) -> Vec<String> {
        self.catalog.servers()
    }

    /// Stop: close the relay and every local connection (stdio servers are
    /// stopped).
    pub async fn shutdown(self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            let _ = task.await;
        }
        self.relay.shutdown().await;
    }
}
