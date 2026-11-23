//! What an `McpApiServer` negotiates, and what its replies then look like
//! **on the wire**, revision by revision.
//!
//! Every assertion here is against `serde_json::Value`, not against the Rust
//! types: the fields a revision requires exist on rmcp's result types as
//! `Option`s that serde skips when they are `None`, so a reply can be well
//! typed and still short of a required field. Only the JSON shows it.
//!
//! The server is driven over an in-memory sink/stream transport with
//! hand-written JSON-RPC frames — the same bytes a client sends — so no HTTP
//! stack is needed to see the shape.
//!
//! What `2026-07-28` requires, and each is asserted below:
//! - no `initialize`: the lifecycle rides in every request's `_meta`
//!   (`io.modelcontextprotocol/protocolVersion`, `…/clientInfo`,
//!   `…/clientCapabilities`), and a request without it is refused;
//! - `server/discover` answers the versions, capabilities and server info,
//!   with `resultType`, `ttlMs` and `cacheScope`;
//! - every result carries `resultType: "complete"`;
//! - `tools/list` carries `ttlMs` and `cacheScope`;
//! - `notifications/tools/list_changed` reaches a client only through
//!   `subscriptions/listen`, tagged with the subscription's id.
//!
//! Older revisions keep the legacy shape: none of those fields.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{
    channel::mpsc::{self, UnboundedReceiver, UnboundedSender},
    SinkExt, StreamExt,
};
use rmcp::{
    model::{ClientJsonRpcMessage, ProtocolVersion, ServerJsonRpcMessage},
    serve_server,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use simply_api::{
    api_service,
    mcp::{supported_protocol_versions, McpApiServer, Tiers, MAX_PROTOCOL_VERSION},
    ApiResult,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Levels {
    /// Peak level per channel, dBFS.
    pub peaks: Vec<f32>,
}

/// A stand-in for an app's api, in two tiers.
#[api_service]
#[async_trait]
pub trait ShapeApi: Send + Sync {
    /// Current levels.
    #[api(read_only, tier = "perform")]
    async fn levels(&self) -> ApiResult<Levels>;

    /// Echo some text back.
    #[api(tier = "edit")]
    async fn echo(
        &self,
        /// What to echo.
        text: String,
    ) -> ApiResult<String>;
}

struct Shape;

#[async_trait]
impl ShapeApi for Shape {
    async fn levels(&self) -> ApiResult<Levels> {
        Ok(Levels { peaks: vec![-6.0, -7.5] })
    }
    async fn echo(&self, text: String) -> ApiResult<String> {
        Ok(text)
    }
}

fn server() -> McpApiServer<Shape> {
    McpApiServer::new(Arc::new(Shape), shape_api_tool_router::<Shape>())
}

/// A client's end of a running server: frames in, frames out, as JSON.
struct Wire {
    to_server: UnboundedSender<ClientJsonRpcMessage>,
    from_server: UnboundedReceiver<ServerJsonRpcMessage>,
}

impl Wire {
    /// Serve `server` until this end is dropped.
    fn open<A: Send + Sync + 'static>(server: McpApiServer<A>) -> Self {
        let (to_client, from_server) = mpsc::unbounded::<ServerJsonRpcMessage>();
        let (to_server, from_client) = mpsc::unbounded::<ClientJsonRpcMessage>();
        tokio::spawn(async move {
            // The handshake ends with an `Err` when the client goes away
            // first; neither is what a test checks.
            if let Ok(running) = serve_server(server, (to_client, from_client)).await {
                let _ = running.waiting().await;
            }
        });
        Self { to_server, from_server }
    }

    async fn send(&mut self, frame: Value) {
        let frame = serde_json::from_value::<ClientJsonRpcMessage>(frame).expect("a client frame");
        self.to_server.send(frame).await.expect("the frame is sent");
    }

    async fn next(&mut self) -> Value {
        let frame = tokio::time::timeout(Duration::from_secs(5), self.from_server.next())
            .await
            .expect("the server answers within 5s")
            .expect("the server is still there");
        serde_json::to_value(frame).expect("json")
    }

    async fn request(&mut self, frame: Value) -> Value {
        self.send(frame).await;
        self.next().await
    }

    /// `initialize` at `asked`, then `notifications/initialized`: the reply.
    async fn initialize(&mut self, asked: &str) -> Value {
        let reply = self
            .request(json!({
                "jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {
                    "protocolVersion": asked,
                    "capabilities": {},
                    "clientInfo": {"name": "shape-test", "version": "0"},
                },
            }))
            .await;
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
        reply
    }
}

/// The `_meta` every `2026-07-28` request carries in place of `initialize`.
fn lifecycle() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "shape-test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

fn tools_list(id: u32) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}})
}

fn tools_call(id: u32, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    })
}

/// `frame` with the `2026-07-28` lifecycle in its `params._meta`.
fn inline(mut frame: Value) -> Value {
    frame["params"]["_meta"] = lifecycle();
    frame
}

fn result(reply: &Value) -> &Value {
    assert!(reply.get("error").is_none(), "the reply is an error: {reply}");
    reply.get("result").unwrap_or_else(|| panic!("a reply carries a result: {reply}"))
}

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> =
        value.as_object().expect("an object").keys().map(String::as_str).collect();
    keys.sort();
    keys
}

fn tool_names(listed: &Value) -> Vec<&str> {
    let tools = result(listed)["tools"].as_array().expect("a tools array");
    tools.iter().map(|t| t["name"].as_str().expect("a name")).collect()
}

// ── the revisions agreed to ─────────────────────────────────────────────

/// The ceiling is the newest revision rmcp knows, and every revision under
/// it is served. This fails when rmcp learns a newer one: check what that
/// revision requires on the wire, assert it here, then raise
/// `MAX_PROTOCOL_VERSION`.
#[test]
fn the_ceiling_is_rmcp_s_newest_revision() {
    assert_eq!(MAX_PROTOCOL_VERSION, ProtocolVersion::LATEST, "rmcp knows a newer revision");
    assert_eq!(supported_protocol_versions(), ProtocolVersion::KNOWN_VERSIONS);
    assert!(supported_protocol_versions().contains(&ProtocolVersion::V_2026_07_28));
}

// ── revisions with an `initialize` handshake ────────────────────────────

#[tokio::test]
async fn a_client_asking_for_a_handshake_revision_is_answered_its_own_in_the_legacy_shape() {
    for asked in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
        let mut wire = Wire::open(server());
        let initialized = wire.initialize(asked).await;
        assert_eq!(
            result(&initialized)["protocolVersion"],
            asked,
            "{asked} is echoed, not narrowed: {initialized}"
        );

        let listed = wire.request(tools_list(1)).await;
        assert_eq!(
            keys(result(&listed)),
            ["tools"],
            "the tools/list envelope at {asked}: {listed}"
        );
        assert_eq!(tool_names(&listed), ["echo", "levels"]);

        let called = wire.request(tools_call(2, "levels", json!({}))).await;
        assert_eq!(
            keys(result(&called)),
            ["content", "isError", "structuredContent"],
            "no resultType at {asked}: {called}"
        );
        assert_eq!(result(&called)["structuredContent"], json!({"peaks": [-6.0, -7.5]}));
    }
}

/// `2026-07-28` has no handshake, so `initialize` can never name it: a
/// client asking for it over `initialize` is answered the newest revision
/// that has one, and gets that revision's shape.
#[tokio::test]
async fn initialize_asking_for_2026_07_28_is_answered_the_newest_handshake_revision() {
    let mut wire = Wire::open(server());
    let initialized = wire.initialize("2026-07-28").await;
    assert_eq!(
        result(&initialized)["protocolVersion"],
        ProtocolVersion::LATEST_WITH_INITIALIZE.as_str(),
        "{initialized}"
    );
    let listed = wire.request(tools_list(1)).await;
    assert_eq!(keys(result(&listed)), ["tools"], "the legacy envelope: {listed}");
}

// ── a tool's output, revision by revision ───────────────────────────────

/// How a channel strip is routed: serializes as a bare JSON string.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Mono,
    Stereo,
}

/// One tool per shape of result: an object, an array, a string-valued
/// type, an `Option` of an object (an `anyOf` schema), and a plain `String`
/// (text, by design).
#[api_service]
#[async_trait]
pub trait OutputApi: Send + Sync {
    /// An object.
    #[api(read_only)]
    async fn levels(&self) -> ApiResult<Levels>;
    /// An array.
    #[api(read_only)]
    async fn names(&self) -> ApiResult<Vec<String>>;
    /// A string-valued type.
    #[api(read_only)]
    async fn mode(&self) -> ApiResult<Mode>;
    /// An object or nothing.
    #[api(read_only)]
    async fn maybe(&self) -> ApiResult<Option<Levels>>;
    /// A plain string: text content, never structured.
    #[api(read_only)]
    async fn greeting(&self) -> ApiResult<String>;
}

struct Outputs;

#[async_trait]
impl OutputApi for Outputs {
    async fn levels(&self) -> ApiResult<Levels> {
        Ok(Levels { peaks: vec![-6.0] })
    }
    async fn names(&self) -> ApiResult<Vec<String>> {
        Ok(vec!["a".into(), "b".into()])
    }
    async fn mode(&self) -> ApiResult<Mode> {
        Ok(Mode::Stereo)
    }
    async fn maybe(&self) -> ApiResult<Option<Levels>> {
        Ok(Some(Levels { peaks: vec![-6.0] }))
    }
    async fn greeting(&self) -> ApiResult<String> {
        Ok("hello".into())
    }
}

/// What one client sees of each tool at `revision`: the top-level `type` of
/// its advertised output schema (`"none"` when none is advertised, `"anyOf"`
/// for an `anyOf` root), its `structuredContent` (`"absent"` when there is
/// none), and the text of its first content block.
async fn outputs_seen_at(revision: &str) -> Value {
    let server = McpApiServer::new(Arc::new(Outputs), output_api_tool_router::<Outputs>());
    let mut wire = Wire::open(server);
    let modern = revision == "2026-07-28";
    let frame = |frame: Value| if modern { inline(frame) } else { frame };
    if !modern {
        let initialized = wire.initialize(revision).await;
        assert_eq!(result(&initialized)["protocolVersion"], revision);
    }

    let listed = wire.request(frame(tools_list(1))).await;
    let mut seen = serde_json::Map::new();
    for (id, tool) in (2..).zip(result(&listed)["tools"].as_array().expect("tools")) {
        let name = tool["name"].as_str().expect("a name");
        let called = wire.request(frame(tools_call(id, name, json!({})))).await;
        let called = result(&called);
        seen.insert(
            name.to_owned(),
            json!({
                "schema": match tool.get("outputSchema") {
                    None => json!("none"),
                    Some(schema) if schema.get("anyOf").is_some() => json!("anyOf"),
                    Some(schema) => schema["type"].clone(),
                },
                "structured": called.get("structuredContent").cloned().unwrap_or(json!("absent")),
                "text": called["content"][0]["text"],
            }),
        );
    }
    Value::Object(seen)
}

/// **Current behaviour, not a promise.** On every revision before
/// `2026-07-28`, rmcp (3.11, `ServerResult::strip_non_object_output_for_legacy_peer`)
/// drops an output schema whose top-level `type` is not `"object"` and a
/// `structuredContent` that is not a JSON object; the value is still in the
/// first text block. Those revisions allow only objects in both places.
/// Under rmcp 3.4 (simply-api 0.3.0) both were sent to every revision.
#[tokio::test]
async fn a_client_before_2026_07_28_gets_non_object_results_only_as_text() {
    let legacy = json!({
        "levels":   {"schema": "object", "structured": {"peaks": [-6.0]}, "text": "{\"peaks\":[-6.0]}"},
        "names":    {"schema": "none",   "structured": "absent",         "text": "[\"a\",\"b\"]"},
        "mode":     {"schema": "none",   "structured": "absent",         "text": "\"stereo\""},
        "maybe":    {"schema": "none",   "structured": {"peaks": [-6.0]}, "text": "{\"peaks\":[-6.0]}"},
        "greeting": {"schema": "none",   "structured": "absent",         "text": "hello"},
    });
    for revision in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
        assert_eq!(outputs_seen_at(revision).await, legacy, "at {revision}");
    }

    let modern = json!({
        "levels":   {"schema": "object", "structured": {"peaks": [-6.0]}, "text": "{\"peaks\":[-6.0]}"},
        "names":    {"schema": "array",  "structured": ["a", "b"],      "text": "[\"a\",\"b\"]"},
        "mode":     {"schema": "string", "structured": "stereo",        "text": "\"stereo\""},
        "maybe":    {"schema": "anyOf",  "structured": {"peaks": [-6.0]}, "text": "{\"peaks\":[-6.0]}"},
        "greeting": {"schema": "none",   "structured": "absent",         "text": "hello"},
    });
    assert_eq!(outputs_seen_at("2026-07-28").await, modern, "at 2026-07-28");
}

// ── 2026-07-28 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn server_discover_answers_versions_capabilities_and_cache_hints() {
    let mut wire = Wire::open(server());
    let discovered = wire
        .request(inline(
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}}),
        ))
        .await;
    let discovered = result(&discovered);
    assert_eq!(
        keys(discovered),
        ["_meta", "cacheScope", "capabilities", "resultType", "supportedVersions", "ttlMs"],
        "{discovered}"
    );
    assert_eq!(discovered["resultType"], "complete");
    assert_eq!(
        discovered["supportedVersions"],
        json!(["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"])
    );
    assert_eq!(discovered["capabilities"]["tools"]["listChanged"], true);
    assert!(discovered["ttlMs"].is_u64(), "{discovered}");
    assert_eq!(discovered["cacheScope"], "private");
    assert!(
        discovered["_meta"]["io.modelcontextprotocol/serverInfo"]["name"].is_string(),
        "the server's identity rides in _meta: {discovered}"
    );
}

#[tokio::test]
async fn a_2026_07_28_tools_list_carries_result_type_and_cache_hints() {
    let mut wire = Wire::open(server());
    let listed = wire.request(inline(tools_list(1))).await;
    assert_eq!(
        keys(result(&listed)),
        ["cacheScope", "resultType", "tools", "ttlMs"],
        "the 2026-07-28 tools/list envelope: {listed}"
    );
    assert_eq!(result(&listed)["resultType"], "complete");
    // The list changes with the tier setting, so it is fresh for no time and
    // is never shared between clients.
    assert_eq!(result(&listed)["ttlMs"], 0);
    assert_eq!(result(&listed)["cacheScope"], "private");
    assert_eq!(tool_names(&listed), ["echo", "levels"]);
}

#[tokio::test]
async fn a_2026_07_28_tools_call_carries_result_type() {
    let mut wire = Wire::open(server());
    let called = wire.request(inline(tools_call(1, "levels", json!({})))).await;
    assert_eq!(
        keys(result(&called)),
        ["content", "isError", "resultType", "structuredContent"],
        "{called}"
    );
    assert_eq!(result(&called)["resultType"], "complete");
    assert_eq!(result(&called)["structuredContent"], json!({"peaks": [-6.0, -7.5]}));

    // Each request carries its own lifecycle, the second one included.
    let echoed = wire.request(inline(tools_call(2, "echo", json!({"text": "hi"})))).await;
    assert_eq!(result(&echoed)["content"][0]["text"], "hi", "{echoed}");
    assert_eq!(result(&echoed)["resultType"], "complete");
}

#[tokio::test]
async fn a_request_with_neither_initialize_nor_lifecycle_meta_is_refused() {
    let mut wire = Wire::open(server());
    let refused = wire.request(tools_list(1)).await;
    assert_eq!(refused["error"]["code"], -32602, "invalid params: {refused}");
    let message = refused["error"]["message"].as_str().expect("a message");
    assert!(message.contains("io.modelcontextprotocol/protocolVersion"), "{message}");
}

/// A `2026-07-28` client has no session to be told on: it opens
/// `subscriptions/listen`, and a tier change reaches it there, tagged with
/// the subscription's id. When it cancels, it is forgotten.
#[tokio::test]
async fn a_tier_change_reaches_a_2026_07_28_client_through_subscriptions_listen() {
    let tiers = Tiers::new(&SHAPE_API_META, &["perform", "edit"], ["perform"]);
    let mut wire = Wire::open(server().with_tiers(tiers.clone()));

    let listed = wire.request(inline(tools_list(1))).await;
    assert_eq!(tool_names(&listed), ["levels"], "only the floor tier is on");

    let acknowledged = wire
        .request(inline(json!({
            "jsonrpc": "2.0", "id": "watch", "method": "subscriptions/listen",
            "params": {"notifications": {"toolsListChanged": true, "promptsListChanged": true}},
        })))
        .await;
    assert_eq!(
        acknowledged["method"], "notifications/subscriptions/acknowledged",
        "{acknowledged}"
    );
    assert_eq!(
        acknowledged["params"]["notifications"],
        json!({"toolsListChanged": true}),
        "only what the server offers is accepted: {acknowledged}"
    );
    assert_eq!(acknowledged["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"], "watch");
    assert_eq!(tiers.connected(), 1, "the subscription is told of changes");

    tiers.set(["perform", "edit"]);
    tiers.notify().await;
    let changed = wire.next().await;
    assert_eq!(changed["method"], "notifications/tools/list_changed", "{changed}");
    assert_eq!(changed["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"], "watch");

    let listed = wire.request(inline(tools_list(2))).await;
    assert_eq!(tool_names(&listed), ["levels", "echo"], "the floor tier first");

    wire.send(json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": "watch"},
    }))
    .await;
    for _ in 0..100 {
        if tiers.connected() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(tiers.connected(), 0, "a cancelled subscription is forgotten");
}
