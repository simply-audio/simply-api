//! Loopback fakes: local MCP servers (HTTP here; stdio in `tests/stdio.rs`),
//! and Lumina's relay endpoint, which is simply-relay's own test fake.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::{json, Value};
use simply_relay::Backoff;
use simply_relay_proxy::{Options, Pacing, Proxy};

#[path = "../../../simply-relay/tests/common/mod.rs"]
pub mod relay;

pub use relay::{FakeLumina, Mcp, DEVICE, KEY, WAIT};

/// A tool taking anything; read-only when `read_only`.
pub fn tool(name: &str, read_only: bool) -> Tool {
    let schema = Arc::new(json!({"type": "object"}).as_object().unwrap().clone());
    let mut tool = Tool::new(name.to_owned(), format!("{name}."), schema);
    if read_only {
        tool.annotations = Some(ToolAnnotations::new().read_only(true));
    }
    tool
}

/// A local MCP server's state: its tools (change them at will) and the
/// calls that reached it.
#[derive(Clone, Default)]
pub struct Local {
    pub name: String,
    pub tools: Arc<Mutex<Vec<Tool>>>,
    pub calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Local {
    pub fn new(name: &str, tools: Vec<Tool>) -> Self {
        Self { name: name.into(), tools: Arc::new(Mutex::new(tools)), ..Self::default() }
    }

    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }

    pub fn set_tools(&self, tools: Vec<Tool>) {
        *self.tools.lock().unwrap() = tools;
    }
}

impl ServerHandler for Local {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.lock().unwrap().clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        self.calls.lock().unwrap().push((request.name.to_string(), args.clone()));
        if request.name == "fail" {
            return Err(ErrorData::invalid_params("bad input", Some(json!({"field": "x"}))));
        }
        let mut result = CallToolResult::structured(
            json!({"server": self.name, "tool": request.name, "args": args}),
        );
        result.content.push(ContentBlock::text("and a line of text"));
        Ok(result.into())
    }
}

/// `local` served over streamable HTTP at `127.0.0.1:port`, on a runtime of
/// its own so that stopping it closes every connection, as a process that
/// quits does.
pub struct HttpServer {
    runtime: Option<tokio::runtime::Runtime>,
}

impl HttpServer {
    pub fn start(local: &Local, port: u16) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let local = local.clone();
        let listener = {
            let _in = runtime.enter();
            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.set_reuseaddr(true).unwrap();
            socket.bind(([127, 0, 0, 1], port).into()).unwrap();
            socket.listen(64).unwrap()
        };
        runtime.spawn(async move {
            use rmcp::transport::streamable_http_server::{
                session::local::LocalSessionManager, StreamableHttpServerConfig,
                StreamableHttpService,
            };
            let service = StreamableHttpService::new(
                move || Ok(local.clone()),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig::default(),
            );
            let app = axum::Router::new().nest_service("/mcp", service);
            axum::serve(listener, app).await.unwrap();
        });
        Self { runtime: Some(runtime) }
    }

    pub fn stop(mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// A loopback port nothing listens on (yet).
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Fast pacing for tests.
pub fn options() -> Options {
    let backoff = Backoff { min: Duration::from_millis(20), max: Duration::from_millis(200) };
    Options {
        local: Pacing {
            backoff,
            poll: Duration::from_millis(100),
            timeout: Duration::from_secs(2),
        },
        relay: backoff,
    }
}

/// A config dialling `lumina`, linked, with `rest` (consent and servers)
/// appended, written into `dir`.
pub fn config(dir: &std::path::Path, lumina: &FakeLumina, rest: &str) -> std::path::PathBuf {
    let path = dir.join("config.toml");
    let text = format!(
        "relay = \"{}\"\nlabel = \"test-mac\"\n\n{rest}\n\n[device]\nid = \"{DEVICE}\"\nkey = \"{KEY}\"\n",
        lumina.url
    );
    std::fs::write(&path, text).unwrap();
    path
}

/// The proxy, connected to `lumina`, and Lumina's MCP client.
pub async fn connected(path: &std::path::Path, lumina: &FakeLumina) -> (Proxy, Mcp) {
    let proxy = Proxy::start(path, options()).unwrap();
    let (hello, conn) = lumina.accept().await;
    assert!(relay::signed_with(&hello, KEY), "{hello:?}");
    let mcp = conn.welcome(Some("chris")).await;
    (proxy, mcp)
}

/// The names Lumina sees, the relay's own diagnostics left out, sorted.
pub async fn names(mcp: &Mcp) -> Vec<String> {
    let mut names: Vec<String> =
        mcp.tool_names().await.into_iter().filter(|n| !n.starts_with("lumina_")).collect();
    names.sort();
    names
}

/// Wait for Lumina to be told the tools changed, then until it sees `want`
/// (a change can take more than one notification to settle).
pub async fn until_names(mcp: &mut Mcp, want: &[&str]) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let now = names(mcp).await;
        if now == want {
            return;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, mcp.list_changed.recv()).await {
            Ok(Some(())) => {}
            _ => panic!("Lumina never saw {want:?}; last: {now:?}"),
        }
    }
}
