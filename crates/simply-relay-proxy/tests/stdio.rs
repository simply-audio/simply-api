//! Local servers over stdio. The fake server is this test binary itself,
//! run with `--fake-mcp`, so the file runs without libtest (which would
//! write to the fake's stdout).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{config, connected, tool, until_names, FakeLumina, WAIT};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::json;

// ── the fake stdio server ───────────────────────────────────────────────

/// `--fake-mcp tools=a,b [add_after_ms=N] [exit_after_ms=N] [log=FILE]`:
/// serves tools `a` and `b` (read-only), adds `late` after N ms and says so,
/// exits after N ms, and appends a line to FILE each time it starts.
#[derive(Clone)]
struct Fake {
    tools: Arc<Mutex<Vec<Tool>>>,
    add_after: Option<Duration>,
}

impl ServerHandler for Fake {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder().enable_tools().enable_tool_list_changed().build(),
        )
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
        let pid = std::process::id();
        Ok(CallToolResult::structured(json!({"tool": request.name, "pid": pid})).into())
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let Some(after) = self.add_after else { return };
        let tools = self.tools.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            tools.lock().unwrap().push(tool("late", true));
            let _ = context.peer.notify_tool_list_changed().await;
        });
    }
}

async fn fake_server(args: Vec<String>) {
    let arg =
        |key: &str| args.iter().find_map(|a| a.strip_prefix(&format!("{key}=")).map(str::to_owned));
    let ms = |key: &str| arg(key).map(|v| Duration::from_millis(v.parse().unwrap()));
    if let Some(log) = arg("log") {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
        writeln!(file, "start {}", std::process::id()).unwrap();
    }
    if let Some(after) = ms("exit_after_ms") {
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            std::process::exit(0);
        });
    }
    let tools = arg("tools").unwrap_or_default();
    let tools = tools.split(',').filter(|t| !t.is_empty()).map(|t| tool(t, true)).collect();
    let fake = Fake { tools: Arc::new(Mutex::new(tools)), add_after: ms("add_after_ms") };
    let running = fake.serve(rmcp::transport::stdio()).await.unwrap();
    let _ = running.waiting().await;
}

// ── the tests ───────────────────────────────────────────────────────────

/// A config with one stdio server `notes` running the fake with `args`.
fn stdio_config(dir: &std::path::Path, lumina: &FakeLumina, args: &[&str]) -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let mut command = vec![exe.to_string_lossy().into_owned(), "--fake-mcp".to_owned()];
    command.extend(args.iter().map(|a| a.to_string()));
    let command = serde_json::to_string(&command).unwrap();
    config(
        dir,
        lumina,
        &format!("[consent]\nread_only = true\n\n[servers.notes]\ncommand = {command}\n"),
    )
}

async fn a_stdio_servers_tools_are_served_and_called() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let path = stdio_config(dir.path(), &lumina, &["tools=search,read"]);
    let (proxy, mut mcp) = connected(&path, &lumina).await;
    until_names(&mut mcp, &["notes.read", "notes.search"]).await;
    let answer = mcp.call("notes.search", json!({"q": "x"}), "?").await.unwrap();
    assert_eq!(answer.structured_content.as_ref().unwrap()["tool"], "search");
    proxy.shutdown().await;
}

async fn a_stdio_servers_list_changed_reaches_lumina() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let path = stdio_config(dir.path(), &lumina, &["tools=search", "add_after_ms=300"]);
    // Polling slower than the wait: only the server's notification can
    // bring `late` in time.
    let mut options = common::options();
    options.local.poll = Duration::from_secs(60);
    let proxy = simply_relay_proxy::Proxy::start(&path, options).unwrap();
    let (_, conn) = lumina.accept().await;
    let mut mcp = conn.welcome(None).await;
    until_names(&mut mcp, &["notes.search"]).await;
    tokio::time::timeout(WAIT, mcp.list_changed.recv()).await.unwrap().unwrap();
    until_names(&mut mcp, &["notes.late", "notes.search"]).await;
    proxy.shutdown().await;
}

async fn a_stdio_server_that_exits_loses_its_tools_and_is_started_again() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let log = dir.path().join("starts.log");
    let log_arg = format!("log={}", log.display());
    let path = stdio_config(dir.path(), &lumina, &["tools=search", "exit_after_ms=800", &log_arg]);
    let (proxy, mut mcp) = connected(&path, &lumina).await;
    until_names(&mut mcp, &["notes.search"]).await;
    let first = mcp.call("notes.search", json!({}), "?").await.unwrap();

    // It exits: the tools go, Lumina is told.
    until_names(&mut mcp, &[]).await;
    // Started again: back, as a new process.
    until_names(&mut mcp, &["notes.search"]).await;
    let second = mcp.call("notes.search", json!({}), "?").await.unwrap();
    let pid = |r: &CallToolResult| r.structured_content.as_ref().unwrap()["pid"].clone();
    assert_ne!(pid(&first), pid(&second));
    let starts = std::fs::read_to_string(&log).unwrap();
    assert!(starts.lines().count() >= 2, "{starts}");
    proxy.shutdown().await;
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    if args.first().map(String::as_str) == Some("--fake-mcp") {
        runtime.block_on(fake_server(args[1..].to_vec()));
        return;
    }
    // `cargo test -- --list` and filters: run everything, list nothing.
    if args.iter().any(|a| a == "--list") {
        return;
    }
    macro_rules! run {
        ($($test:ident),*) => {{
            let tests: &[&str] = &[$(stringify!($test)),*];
            println!("\nrunning {} tests", tests.len());
            $(
                runtime.block_on($test());
                println!("test {} ... ok", stringify!($test));
            )*
            println!("\ntest result: ok. {} passed; 0 failed\n", tests.len());
        }};
    }
    run!(
        a_stdio_servers_tools_are_served_and_called,
        a_stdio_servers_list_changed_reaches_lumina,
        a_stdio_server_that_exits_loses_its_tools_and_is_started_again
    );
}
