//! The one MCP server Lumina sees: the tools of every local server that is
//! up, each named `<server>.<tool>`, with calls routed to the server that
//! owns the tool and its answer passed back as it came.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{NotificationContext, Peer, RequestContext, RoleClient, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceError};
use simply_relay::Class;

/// What separates a server's name from its tool's in the names Lumina sees.
pub const SEPARATOR: char = '.';

/// A local server that is up: the client connected to it and its tools.
struct Up {
    peer: Peer<RoleClient>,
    tools: Vec<Tool>,
}

/// The local servers that are up, and the Lumina connection to tell when
/// that changes.
#[derive(Default)]
pub struct Catalog {
    up: Mutex<BTreeMap<String, Up>>,
    /// Per server, the tools the config says are read-only.
    read_only: BTreeMap<String, BTreeSet<String>>,
    lumina: Mutex<Option<Peer<RoleServer>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

impl Catalog {
    pub fn new(read_only: BTreeMap<String, BTreeSet<String>>) -> Arc<Self> {
        Arc::new(Self { read_only, ..Self::default() })
    }

    /// `server` is up with `tools`. Lumina is told if that changes what it
    /// sees.
    pub fn up(&self, server: &str, peer: Peer<RoleClient>, tools: Vec<Tool>) {
        let changed = {
            let mut up = lock(&self.up);
            let changed = up.get(server).map_or(&[][..], |was| &was.tools[..]) != tools;
            up.insert(server.to_owned(), Up { peer, tools });
            changed
        };
        if changed {
            tracing::info!("proxy: {server} is up");
            self.notify();
        }
    }

    /// `server` is down: its tools are gone.
    pub fn down(&self, server: &str) {
        if lock(&self.up).remove(server).is_some_and(|was| !was.tools.is_empty()) {
            tracing::info!("proxy: {server} is down");
            self.notify();
        }
    }

    /// The servers that are up.
    pub fn servers(&self) -> Vec<String> {
        lock(&self.up).keys().cloned().collect()
    }

    /// Every tool that is up, renamed `<server>.<tool>`.
    pub fn tools(&self) -> Vec<Tool> {
        let up = lock(&self.up);
        up.iter().flat_map(|(server, up)| up.tools.iter().map(|t| renamed(server, t))).collect()
    }

    fn find(&self, name: &str) -> Option<(String, Peer<RoleClient>, Tool)> {
        let (server, tool) = name.split_once(SEPARATOR)?;
        let up = lock(&self.up);
        let entry = up.get(server)?;
        let found = entry.tools.iter().find(|t| t.name == tool)?;
        Some((server.to_owned(), entry.peer.clone(), found.clone()))
    }

    /// The class of the tool Lumina knows as `name`: read-only when the
    /// config lists it or the server marks it `readOnlyHint`, otherwise an
    /// action under its server's switch. `None` when no server that is up
    /// has it.
    pub fn classify(&self, name: &str) -> Option<Class> {
        let (server, _, tool) = self.find(name)?;
        let listed = self.read_only.get(&server).is_some_and(|tools| tools.contains(&*tool.name));
        let hinted = tool.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true);
        Some(if listed || hinted { Class::ReadOnly } else { Class::Action(server) })
    }

    fn set_lumina(&self, peer: Peer<RoleServer>) {
        *lock(&self.lumina) = Some(peer);
    }

    /// Tell a connected Lumina to list the tools again.
    fn notify(&self) {
        let Some(peer) = lock(&self.lumina).clone() else { return };
        tokio::spawn(async move {
            let _ =
                tokio::time::timeout(Duration::from_secs(5), peer.notify_tool_list_changed()).await;
        });
    }
}

fn renamed(server: &str, tool: &Tool) -> Tool {
    let mut tool = tool.clone();
    tool.name = format!("{server}{SEPARATOR}{}", tool.name).into();
    tool
}

/// The handler served over the relay, one per Lumina connection, all on the
/// same [`Catalog`].
#[derive(Clone)]
pub struct Union {
    catalog: Arc<Catalog>,
}

impl Union {
    pub fn new(catalog: Arc<Catalog>) -> Self {
        Self { catalog }
    }
}

impl ServerHandler for Union {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_tool_list_changed().build())
            .with_server_info(Implementation::new(crate::config::APP, env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.catalog.tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        let (server, _, tool) = self.catalog.find(name)?;
        Some(renamed(&server, &tool))
    }

    async fn call_tool(
        &self,
        mut request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some((server, peer, tool)) = self.catalog.find(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("no local server has {} now", request.name),
                None,
            ));
        };
        request.name = tool.name;
        match peer.call_tool_once(request).await {
            Ok(response) => Ok(response),
            Err(ServiceError::McpError(error)) => Err(error),
            Err(error) => {
                Err(ErrorData::internal_error(format!("{server} did not answer: {error}"), None))
            }
        }
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.catalog.set_lumina(context.peer);
    }
}
