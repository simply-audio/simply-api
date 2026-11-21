//! One local MCP server, kept connected: connect (or spawn), list its tools,
//! follow them while it is up, and when it goes, take its tools away and
//! try again with backoff.
//!
//! A server is down from the moment a listing fails or its connection
//! closes (a stdio server exited). Its tools are re-listed when it says
//! `notifications/tools/list_changed` and on every poll, since a
//! streamable-HTTP server may have no way to push one.

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{ClientHandler, ServiceExt};
use simply_relay::Backoff;
use tokio::sync::mpsc;

use crate::config::Transport;
use crate::union::Catalog;

/// How the proxy paces its local servers.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    /// Between attempts to reach a server that is down.
    pub backoff: Backoff,
    /// Between listings of a server that is up.
    pub poll: Duration,
    /// How long a server gets to answer `initialize` or a listing.
    pub timeout: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            backoff: Backoff { min: Duration::from_secs(1), max: Duration::from_secs(30) },
            poll: Duration::from_secs(10),
            timeout: Duration::from_secs(10),
        }
    }
}

/// The proxy as a local server's client: it hears `tools/list_changed`.
struct Listener(mpsc::UnboundedSender<()>);

impl ClientHandler for Listener {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(crate::config::APP, env!("CARGO_PKG_VERSION")),
        )
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        let _ = self.0.send(());
    }
}

async fn connect(
    transport: &Transport,
    listener: Listener,
) -> Result<RunningService<RoleClient, Listener>, String> {
    match transport {
        Transport::Http(url) => {
            let transport = StreamableHttpClientTransport::from_uri(url.as_str());
            listener.serve(transport).await.map_err(|e| e.to_string())
        }
        Transport::Stdio(command) => {
            let mut cmd = tokio::process::Command::new(&command[0]);
            cmd.args(&command[1..]);
            let (child, _) = TokioChildProcess::builder(cmd)
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .map_err(|e| format!("could not run {}: {e}", command[0]))?;
            listener.serve(child).await.map_err(|e| e.to_string())
        }
    }
}

/// `delay` ±25 %.
fn jittered(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    delay.mul_f64(0.75 + f64::from(nanos % 1000) / 2000.0)
}

/// Keep `name` connected for as long as the task runs.
pub async fn supervise(name: String, transport: Transport, catalog: Arc<Catalog>, pacing: Pacing) {
    let mut delay = pacing.backoff.min;
    loop {
        match serve_while_up(&name, &transport, &catalog, pacing).await {
            Ok(()) => delay = pacing.backoff.min,
            Err(error) => tracing::warn!("proxy: {name}: {error}"),
        }
        catalog.down(&name);
        tokio::time::sleep(jittered(delay)).await;
        delay = (delay * 2).min(pacing.backoff.max);
    }
}

/// Connect and follow the server's tools until it goes. `Ok` when it was up
/// at all.
async fn serve_while_up(
    name: &str,
    transport: &Transport,
    catalog: &Catalog,
    pacing: Pacing,
) -> Result<(), String> {
    let (tx, mut changed) = mpsc::unbounded_channel();
    let running = tokio::time::timeout(pacing.timeout, connect(transport, Listener(tx)))
        .await
        .map_err(|_| "did not answer in time".to_owned())??;
    let peer = running.peer().clone();
    let list = || async {
        match tokio::time::timeout(pacing.timeout, peer.list_all_tools()).await {
            Ok(Ok(tools)) => Ok(tools),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("did not list its tools in time".to_owned()),
        }
    };
    let token = running.cancellation_token();
    let first = list().await;
    let result = match first {
        Err(e) => Err(e),
        Ok(tools) => {
            catalog.up(name, peer.clone(), tools);
            let waiting = running.waiting();
            tokio::pin!(waiting);
            let mut poll = tokio::time::interval(pacing.poll);
            poll.tick().await;
            loop {
                tokio::select! {
                    _ = &mut waiting => break Ok(()),
                    _ = poll.tick() => {}
                    Some(()) = changed.recv() => {}
                }
                match list().await {
                    Ok(tools) => catalog.up(name, peer.clone(), tools),
                    Err(e) => {
                        tracing::warn!("proxy: {name}: {e}");
                        break Ok(());
                    }
                }
            }
        }
    };
    // Closes the connection; a stdio server's process is killed with it.
    token.cancel();
    result
}
