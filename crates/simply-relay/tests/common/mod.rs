//! A scripted stand-in for Lumina's listener, and a small app handler.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient, RoleServer, RunningService};
use rmcp::{ClientHandler, ErrorData, ServerHandler, ServiceExt};
use serde_json::{json, Value};
use simply_relay::frames::{json_frames, FrameSink, FrameStream};
use simply_relay::wire::{AppFrame, Hello, LuminaFrame, Refused, RefusedReason, Welcome};
use simply_relay::{Backoff, Builder, DeviceKey, Relay, Secret};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

pub const WAIT: Duration = Duration::from_secs(5);

// ── the app's handler ───────────────────────────────────────────────────

/// Four tools: `levels` (no argument), `echo` (one required argument),
/// `open_session` (an action) and `wipe`, which no test declares. Every call
/// that reaches it is recorded.
#[derive(Clone, Default)]
pub struct Studio {
    pub reached: Arc<Mutex<Vec<String>>>,
}

impl Studio {
    pub fn reached(&self) -> Vec<String> {
        self.reached.lock().unwrap().clone()
    }
}

fn schema(v: Value) -> Arc<serde_json::Map<String, Value>> {
    Arc::new(v.as_object().unwrap().clone())
}

impl ServerHandler for Studio {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_prompts().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let none = || schema(json!({"type": "object", "properties": {}}));
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("levels", "Levels.", none()),
            Tool::new(
                "echo",
                "Echo.",
                schema(
                    json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
                ),
            ),
            Tool::new("open_session", "Open a session.", none()),
            Tool::new("wipe", "Wipe everything.", none()),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.reached.lock().unwrap().push(request.name.to_string());
        let result = match request.name.as_ref() {
            "levels" => CallToolResult::structured(json!({"rate": 48000, "peaks": [-6.0]})),
            "echo" => CallToolResult::success(vec![ContentBlock::text(
                request
                    .arguments
                    .and_then(|a| a.get("text").cloned())
                    .unwrap_or_default()
                    .to_string(),
            )]),
            other => CallToolResult::success(vec![ContentBlock::text(format!("{other} done"))]),
        };
        Ok(result.into())
    }
}

// ── the app's keychain ──────────────────────────────────────────────────

pub const DEVICE: &str = "0123456789abcdef0123456789abcdef";
pub const KEY: &str = "3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f";
pub const NEW_KEY: &str = "4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e";

/// Where the test app keeps its device key: what the key callback reads and
/// the new-key callback writes. `refuse_new` makes the write fail.
#[derive(Clone, Default)]
pub struct Keychain {
    pub key: Arc<Mutex<Option<DeviceKey>>>,
    pub refuse_new: Arc<Mutex<bool>>,
}

impl Keychain {
    pub fn linked() -> Self {
        let keychain = Self::default();
        keychain.set(KEY);
        keychain
    }

    pub fn set(&self, key: &str) {
        *self.key.lock().unwrap() =
            Some(DeviceKey { device_id: DEVICE.into(), key: Secret::new(key) });
    }

    pub fn key(&self) -> Option<String> {
        self.key.lock().unwrap().as_ref().map(|d| d.key.expose().to_owned())
    }

    /// The key and new-key callbacks on `builder`.
    pub fn on(&self, builder: Builder) -> Builder {
        let (read, write) = (self.clone(), self.clone());
        builder.key(move || read.key.lock().unwrap().clone()).on_new_key(move |device| {
            if *write.refuse_new.lock().unwrap() {
                return Err("the keychain is locked".into());
            }
            *write.key.lock().unwrap() = Some(device.clone());
            Ok(())
        })
    }
}

/// A relay for "studio" in `dir`, dialling `lumina`, with fast pacing and
/// its key in `keychain`.
pub fn relay(dir: &std::path::Path, lumina: &FakeLumina, keychain: &Keychain) -> Builder {
    let builder = Relay::builder("studio", "1.2.3", dir);
    keychain
        .on(builder)
        .endpoint(lumina.url.clone())
        .label("test-mac")
        .read_only(["levels", "echo"])
        .action("session", ["open_session"])
        .backoff(Backoff { min: Duration::from_millis(20), max: Duration::from_millis(200) })
        .handshake_timeout(Duration::from_secs(2))
}

// ── Lumina's side ───────────────────────────────────────────────────────

pub struct FakeLumina {
    listener: TcpListener,
    pub url: String,
}

/// An accepted socket after the app's first frame.
pub struct Conn {
    sink: FrameSink<TcpStream, LuminaFrame>,
    stream: FrameStream<TcpStream, AppFrame>,
}

impl FakeLumina {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/devices/v1", listener.local_addr().unwrap());
        Self { listener, url }
    }

    /// The next device's hello.
    pub async fn accept(&self) -> (Hello, Conn) {
        let (tcp, _) = tokio::time::timeout(WAIT, self.listener.accept())
            .await
            .expect("a device dials within the wait")
            .unwrap();
        let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let (sink, mut stream) = json_frames::<_, LuminaFrame, AppFrame>(ws, None);
        let first =
            tokio::time::timeout(WAIT, stream.next()).await.unwrap().expect("a first frame");
        let AppFrame::Hello(hello) = first;
        (hello, Conn { sink, stream })
    }

    /// No device dials within `wait`.
    pub async fn quiet_for(&self, wait: Duration) -> bool {
        tokio::time::timeout(wait, self.listener.accept()).await.is_err()
    }
}

/// Does `hello` carry a good signature under `key`, signed about now?
pub fn signed_with(hello: &Hello, key: &str) -> bool {
    use simply_relay::wire::verify_hello;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    hello.timestamp.abs_diff(now) <= 5
        && verify_hello(key, hello.timestamp, &hello.device_id, &hello.signature)
}

/// What a connected MCP client saw.
pub struct Mcp {
    pub client: RunningService<RoleClient, Watcher>,
    pub list_changed: mpsc::UnboundedReceiver<()>,
}

pub struct Watcher(mpsc::UnboundedSender<()>);

impl ClientHandler for Watcher {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        let _ = self.0.send(());
    }
}

impl Conn {
    pub async fn refuse(mut self, reason: RefusedReason, message: &str) {
        let refused = LuminaFrame::Refused(Refused { reason, message: message.into() });
        self.sink.send(refused).await.unwrap();
        let _ = self.sink.close().await;
    }

    /// Welcome the device as `member` and connect to it as Lumina's MCP
    /// client.
    pub async fn welcome(self, member: Option<&str>) -> Mcp {
        self.welcome_with(member, None).await.expect("the device serves MCP")
    }

    /// Welcome the device, handing it `key` when given (a re-keyed device),
    /// and try to connect as Lumina's MCP client.
    pub async fn welcome_with(
        mut self,
        member: Option<&str>,
        key: Option<&str>,
    ) -> Result<Mcp, String> {
        let welcome = Welcome {
            device_id: DEVICE.into(),
            member: member.map(Into::into),
            key: key.map(Into::into),
        };
        self.sink.send(LuminaFrame::Welcome(welcome)).await.unwrap();
        let (tx, list_changed) = mpsc::unbounded_channel();
        let transport = (
            self.sink.retype::<rmcp::model::ClientJsonRpcMessage>(),
            self.stream.retype::<rmcp::model::ServerJsonRpcMessage>(),
        );
        let client = Watcher(tx).serve(transport).await.map_err(|e| e.to_string())?;
        Ok(Mcp { client, list_changed })
    }
}

impl Mcp {
    pub async fn tools(&self) -> Vec<Tool> {
        self.client.list_all_tools().await.unwrap()
    }

    pub async fn tool_names(&self) -> Vec<String> {
        self.tools().await.into_iter().map(|t| t.name.to_string()).collect()
    }

    /// Call `tool` with `purpose` in `_meta`.
    pub async fn call(
        &self,
        tool: &str,
        args: Value,
        purpose: &str,
    ) -> Result<CallToolResult, ErrorData> {
        let mut params = CallToolRequestParams::new(tool.to_owned());
        if let Value::Object(args) = args {
            params.arguments = Some(args);
        }
        let mut meta = rmcp::model::RequestMetaObject::default();
        meta.0 .0.insert(simply_relay::wire::PURPOSE_META_KEY.into(), purpose.into());
        params.meta = Some(meta);
        match self.client.call_tool(params).await {
            Ok(result) => Ok(result),
            Err(rmcp::ServiceError::McpError(e)) => Err(e),
            Err(e) => panic!("transport error: {e}"),
        }
    }
}

/// Wait until the relay's status matches.
pub async fn until(
    relay: &Relay,
    what: impl Fn(&simply_relay::Status) -> bool,
) -> simply_relay::Status {
    let mut watch = relay.watch_status();
    let status = tokio::time::timeout(WAIT, watch.wait_for(|s| what(s)))
        .await
        .unwrap_or_else(|_| panic!("status never matched; last: {:?}", relay.status()))
        .unwrap()
        .clone();
    status
}
