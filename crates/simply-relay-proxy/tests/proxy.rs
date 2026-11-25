//! The proxy end to end over loopback: local streamable-HTTP MCP servers on
//! one side, a scripted Lumina on the other.

mod common;

use common::{
    config, connected, free_port, names, tool, until_names, FakeLumina, HttpServer, Local, WAIT,
};
use serde_json::json;
use simply_relay::Status;

#[tokio::test]
async fn lumina_sees_every_servers_tools_by_server_and_calls_reach_the_right_one() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let alpha = Local::new("alpha", vec![tool("read", true), tool("write", false)]);
    let beta = Local::new(
        "beta",
        vec![tool("read", true), tool("wipe", false), tool("search", false), tool("fail", true)],
    );
    let (alpha_port, beta_port) = (free_port(), free_port());
    let _alpha_server = HttpServer::start(&alpha, alpha_port);
    let _beta_server = HttpServer::start(&beta, beta_port);
    let path = config(
        dir.path(),
        &lumina,
        &format!(
            "[consent]\nread_only = true\nactions = {{ alpha = true }}\n\n\
             [servers.alpha]\nurl = \"http://127.0.0.1:{alpha_port}/mcp\"\n\n\
             [servers.beta]\nurl = \"http://127.0.0.1:{beta_port}/mcp\"\nread_only = [\"search\"]\n"
        ),
    );
    let proxy = proxy_with_hello_check(&path, &lumina).await;
    let (proxy, mut mcp) = proxy;

    // beta's `wipe` is an action under beta's switch, which is off; its
    // `search` is read-only because the config says so.
    until_names(&mut mcp, &["alpha.read", "alpha.write", "beta.fail", "beta.read", "beta.search"])
        .await;
    let tools = mcp.tools().await;
    let class = |name: &str| {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let hint = tool.annotations.as_ref().and_then(|a| a.read_only_hint);
        let class = tool.meta.as_ref().and_then(|m| m.0.get("lumina/class").cloned());
        (hint, class)
    };
    assert_eq!(class("alpha.read"), (Some(true), Some(json!("read_only"))));
    assert_eq!(class("alpha.write"), (Some(false), Some(json!("alpha"))));
    assert_eq!(class("beta.search"), (Some(true), Some(json!("read_only"))));
    let described = tools.iter().find(|t| t.name == "alpha.write").unwrap();
    assert_eq!(described.description.as_deref(), Some("write."), "passed through as listed");

    // A call reaches the server that owns the tool, under its own name, and
    // its answer comes back as it was given.
    let answer = mcp.call("alpha.write", json!({"take": 3}), "jam #studio").await.unwrap();
    assert_eq!(
        answer.structured_content,
        Some(json!({"server": "alpha", "tool": "write", "args": {"take": 3}}))
    );
    assert_eq!(answer.content.len(), 2);
    assert_eq!(answer.content[1].as_text().unwrap().text, "and a line of text");
    mcp.call("beta.read", json!({}), "?").await.unwrap();
    assert_eq!(alpha.calls(), [("write".to_owned(), json!({"take": 3}))]);
    assert_eq!(beta.calls(), [("read".to_owned(), json!({}))]);

    // A server's own error comes back as its error.
    let failed = mcp.call("beta.fail", json!({}), "?").await.unwrap_err();
    assert_eq!((failed.message.as_ref(), failed.data), ("bad input", Some(json!({"field": "x"}))));

    // What the switches refuse never reaches the server.
    let refused = mcp.call("beta.wipe", json!({}), "?").await.unwrap_err();
    assert_eq!(refused.data, Some(json!({"refused": "switched_off", "class": "beta"})));
    let unknown = mcp.call("gamma.read", json!({}), "?").await.unwrap_err();
    assert_eq!(unknown.data, Some(json!({"refused": "not_exposed", "class": null})));
    assert_eq!(beta.calls().len(), 2, "read and fail only");

    proxy.shutdown().await;
}

/// Start the proxy and check the hello says who it is and its switches.
async fn proxy_with_hello_check(
    path: &std::path::Path,
    lumina: &FakeLumina,
) -> (simply_relay_proxy::Proxy, common::Mcp) {
    let proxy = simply_relay_proxy::Proxy::start(path, common::options()).unwrap();
    let (hello, conn) = lumina.accept().await;
    assert_eq!((hello.app.as_str(), hello.label.as_str()), ("simply-relay-proxy", "test-mac"));
    assert!(common::relay::signed_with(&hello, common::KEY));
    assert!(hello.consent.read_only);
    assert_eq!(hello.consent.actions.get("alpha"), Some(&true));
    assert_eq!(hello.consent.actions.get("beta"), Some(&false));
    (proxy, conn.welcome(Some("chris")).await)
}

#[tokio::test]
async fn servers_that_come_and_go_are_announced_and_a_dead_ones_tools_vanish() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let alpha = Local::new("alpha", vec![tool("read", true)]);
    let beta = Local::new("beta", vec![tool("status", true)]);
    let (alpha_port, beta_port) = (free_port(), free_port());
    let alpha_server = HttpServer::start(&alpha, alpha_port);
    let path = config(
        dir.path(),
        &lumina,
        &format!(
            "[consent]\nread_only = true\n\n\
             [servers.alpha]\nurl = \"http://127.0.0.1:{alpha_port}/mcp\"\n\n\
             [servers.beta]\nurl = \"http://127.0.0.1:{beta_port}/mcp\"\n"
        ),
    );
    let (proxy, mut mcp) = connected(&path, &lumina).await;
    until_names(&mut mcp, &["alpha.read"]).await;

    // beta starts: Lumina is told, and sees its tools.
    let beta_server = HttpServer::start(&beta, beta_port);
    tokio::time::timeout(WAIT, mcp.list_changed.recv()).await.unwrap().unwrap();
    until_names(&mut mcp, &["alpha.read", "beta.status"]).await;

    // beta changes its tools: picked up and announced.
    beta.set_tools(vec![tool("status", true), tool("levels", true)]);
    tokio::time::timeout(WAIT, mcp.list_changed.recv()).await.unwrap().unwrap();
    until_names(&mut mcp, &["alpha.read", "beta.levels", "beta.status"]).await;

    // alpha dies: its tools disappear rather than fail, and Lumina is told.
    alpha_server.stop();
    tokio::time::timeout(WAIT, mcp.list_changed.recv()).await.unwrap().unwrap();
    until_names(&mut mcp, &["beta.levels", "beta.status"]).await;
    assert_eq!(proxy.servers_up(), ["beta"]);
    let gone = mcp.call("alpha.read", json!({}), "?").await.unwrap_err();
    assert_eq!(gone.data, Some(json!({"refused": "not_exposed", "class": null})));

    // alpha comes back on its port: retried, and its tools return.
    let _alpha_server = HttpServer::start(&alpha, alpha_port);
    until_names(&mut mcp, &["alpha.read", "beta.levels", "beta.status"]).await;
    mcp.call("alpha.read", json!({}), "?").await.unwrap();
    assert_eq!(alpha.calls().len(), 1);

    beta_server.stop();
    until_names(&mut mcp, &["alpha.read"]).await;
    proxy.shutdown().await;
}

#[tokio::test]
async fn the_proxy_dials_lumina_again_when_the_relay_drops() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let alpha = Local::new("alpha", vec![tool("read", true)]);
    let port = free_port();
    let _server = HttpServer::start(&alpha, port);
    let path = config(
        dir.path(),
        &lumina,
        &format!("[consent]\nread_only = true\n\n[servers.alpha]\nurl = \"http://127.0.0.1:{port}/mcp\"\n"),
    );
    let (proxy, mut mcp) = connected(&path, &lumina).await;
    until_names(&mut mcp, &["alpha.read"]).await;

    // Lumina goes away; the proxy dials again with a fresh signed hello and
    // serves the same tools on the new connection.
    mcp.client.cancel().await.unwrap();
    let (hello, conn) = lumina.accept().await;
    assert!(common::relay::signed_with(&hello, common::KEY));
    let mut mcp = conn.welcome(Some("chris")).await;
    common::relay::until(proxy.relay(common::DEVICE).unwrap(), |s| {
        matches!(s, Status::Connected { .. })
    })
    .await;
    until_names(&mut mcp, &["alpha.read"]).await;
    mcp.call("alpha.read", json!({}), "?").await.unwrap();

    // A refused handshake backs off and tries again.
    mcp.client.cancel().await.unwrap();
    let (_, conn) = lumina.accept().await;
    drop(conn);
    let (_, conn) = lumina.accept().await;
    let mcp = conn.welcome(None).await;
    assert_eq!(names(&mcp).await, ["alpha.read"]);
    proxy.shutdown().await;
}

#[tokio::test]
async fn a_new_key_from_lumina_is_written_into_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let path = config(dir.path(), &lumina, "# hand-written\n[consent]\nread_only = true\n");
    let proxy = simply_relay_proxy::Proxy::start(&path, common::options()).unwrap();
    let (_, conn) = lumina.accept().await;
    let _mcp = conn.welcome_with(None, Some(common::relay::NEW_KEY)).await.unwrap();
    let config = simply_relay_proxy::Config::load(&path).unwrap();
    assert_eq!(config.device.unwrap().key, common::relay::NEW_KEY);
    assert!(std::fs::read_to_string(&path).unwrap().contains("# hand-written"));
    proxy.shutdown().await;

    // Zero linked devices: starts cleanly without crashing or dialling.
    let unlinked = dir.path().join("unlinked.toml");
    std::fs::write(&unlinked, format!("relay = \"{}\"\n", lumina.url)).unwrap();
    let proxy = simply_relay_proxy::Proxy::start(&unlinked, common::options()).unwrap();
    assert!(proxy.relays().is_empty());
    assert!(lumina.quiet_for(std::time::Duration::from_millis(200)).await);
    proxy.shutdown().await;
}

#[tokio::test]
async fn multiple_linked_devices_share_local_servers_with_independent_consent_and_keys() {
    const DEV_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const KEY_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const DEV_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const KEY_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let alpha = Local::new("alpha", vec![tool("read", true), tool("write", false)]);
    let beta = Local::new("beta", vec![tool("status", true)]);
    let (alpha_port, beta_port) = (free_port(), free_port());
    let _alpha_server = HttpServer::start(&alpha, alpha_port);

    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            "# shared config\n\
             relay = \"{}\"\n\
             label = \"studio-mac\"\n\n\
             [consent]\n\
             read_only = true\n\
             actions = {{ alpha = true }}\n\n\
             [servers.alpha]\n\
             url = \"http://127.0.0.1:{alpha_port}/mcp\"\n\n\
             [servers.beta]\n\
             url = \"http://127.0.0.1:{beta_port}/mcp\"\n\n\
             [[devices]]\n\
             id = \"{DEV_A}\"\n\
             key = \"{KEY_A}\"\n\
             server = \"Server A\"\n\
             member = \"alice\"\n\n\
             [[devices]]\n\
             id = \"{DEV_B}\"\n\
             key = \"{KEY_B}\"\n\
             server = \"Server B\"\n\
             member = \"bob\"\n\
             [devices.consent]\n\
             actions = {{ alpha = false }}\n",
            lumina.url
        ),
    )
    .unwrap();

    let proxy = simply_relay_proxy::Proxy::start(&path, common::options()).unwrap();
    assert_eq!(proxy.relays().len(), 2);

    // Both devices dial in (in any order). Re-key DEV_A in its welcome.
    let mut mcp_a = None;
    let mut mcp_b = None;
    for _ in 0..2 {
        let (hello, conn) = lumina.accept().await;
        match hello.device_id.as_str() {
            DEV_A => {
                assert!(common::relay::signed_with(&hello, KEY_A));
                assert!(hello.consent.read_only);
                assert_eq!(hello.consent.actions.get("alpha"), Some(&true));
                mcp_a = Some(
                    conn.welcome_device(DEV_A, Some("alice"), Some(common::relay::NEW_KEY))
                        .await
                        .unwrap(),
                );
            }
            DEV_B => {
                assert!(common::relay::signed_with(&hello, KEY_B));
                assert!(hello.consent.read_only);
                assert_eq!(hello.consent.actions.get("alpha"), Some(&false));
                mcp_b = Some(conn.welcome_device(DEV_B, Some("bob"), None).await.unwrap());
            }
            other => panic!("unexpected device_id: {other}"),
        }
    }
    let mut mcp_a = mcp_a.unwrap();
    let mut mcp_b = mcp_b.unwrap();

    // Separate per-device state directories exist without clobbering each other.
    assert!(dir.path().join("devices").join(DEV_A).join("lumina.toml").exists());
    assert!(dir.path().join("devices").join(DEV_B).join("lumina.toml").exists());

    // DEV_A sees read + write; DEV_B sees read only (alpha action overridden to false).
    until_names(&mut mcp_a, &["alpha.read", "alpha.write"]).await;
    until_names(&mut mcp_b, &["alpha.read"]).await;

    // Both can call alpha.read on the shared local server.
    mcp_a.call("alpha.read", json!({"from": "a"}), "check a").await.unwrap();
    mcp_b.call("alpha.read", json!({"from": "b"}), "check b").await.unwrap();

    // DEV_A can call alpha.write; DEV_B is refused by its per-device consent.
    mcp_a.call("alpha.write", json!({"n": 1}), "write a").await.unwrap();
    let refused = mcp_b.call("alpha.write", json!({"n": 2}), "write b").await.unwrap_err();
    assert_eq!(refused.data, Some(json!({"refused": "switched_off", "class": "alpha"})));
    assert_eq!(alpha.calls().len(), 3, "DEV_B's refused write never reached alpha");

    // Starting beta notifies BOTH connected devices.
    let _beta_server = HttpServer::start(&beta, beta_port);
    tokio::time::timeout(WAIT, mcp_a.list_changed.recv()).await.unwrap().unwrap();
    tokio::time::timeout(WAIT, mcp_b.list_changed.recv()).await.unwrap().unwrap();
    until_names(&mut mcp_a, &["alpha.read", "alpha.write", "beta.status"]).await;
    until_names(&mut mcp_b, &["alpha.read", "beta.status"]).await;

    // Re-keying DEV_A updated ONLY DEV_A's key; DEV_B's key, metadata, and
    // consent override remain intact.
    let reloaded = simply_relay_proxy::Config::load(&path).unwrap();
    assert_eq!(reloaded.find_device(DEV_A).unwrap().key, common::relay::NEW_KEY);
    assert_eq!(reloaded.find_device(DEV_A).unwrap().server.as_deref(), Some("Server A"));
    let dev_b = reloaded.find_device(DEV_B).unwrap();
    assert_eq!(dev_b.key, KEY_B);
    assert_eq!(dev_b.server.as_deref(), Some("Server B"));
    assert_eq!(dev_b.member.as_deref(), Some("bob"));
    assert_eq!(dev_b.consent.as_ref().unwrap().actions.get("alpha"), Some(&false));
    assert!(std::fs::read_to_string(&path).unwrap().starts_with("# shared config\n"));

    proxy.shutdown().await;
}

/// A `2026-07-28` Lumina has no session to be told on: it listens with
/// `subscriptions/listen`, and a local server coming up reaches it there.
#[tokio::test]
async fn a_2026_07_28_lumina_hears_a_server_come_up_through_subscriptions_listen() {
    use common::relay::{inline, result, RawMcp};
    use serde_json::Value;

    /// The reply to `id`, past any notification sent before it.
    async fn reply(mcp: &mut RawMcp, frame: Value) -> Value {
        let id = frame["id"].clone();
        mcp.send(frame).await;
        loop {
            let next = mcp.next().await;
            if next["id"] == id {
                return next;
            }
        }
    }
    async fn listed(mcp: &mut RawMcp, id: u32) -> Vec<String> {
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}});
        let listed = reply(mcp, inline(frame)).await;
        assert_eq!(result(&listed)["resultType"], "complete", "{listed}");
        let tools = result(&listed)["tools"].as_array().unwrap().iter();
        let mut names: Vec<String> = tools
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .filter(|n| !n.starts_with("lumina_"))
            .collect();
        names.sort();
        names
    }

    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let alpha = Local::new("alpha", vec![tool("read", true)]);
    let beta = Local::new("beta", vec![tool("status", true)]);
    let (alpha_port, beta_port) = (free_port(), free_port());
    let _alpha_server = HttpServer::start(&alpha, alpha_port);
    let path = config(
        dir.path(),
        &lumina,
        &format!(
            "[consent]\nread_only = true\n\n\
             [servers.alpha]\nurl = \"http://127.0.0.1:{alpha_port}/mcp\"\n\n\
             [servers.beta]\nurl = \"http://127.0.0.1:{beta_port}/mcp\"\n"
        ),
    );
    let proxy = simply_relay_proxy::Proxy::start(&path, common::options()).unwrap();
    let (_, conn) = lumina.accept().await;
    let mut mcp = conn.welcome_raw(Some("chris")).await;

    let deadline = tokio::time::Instant::now() + WAIT;
    let mut id = 1;
    while listed(&mut mcp, id).await != ["alpha.read"] {
        assert!(tokio::time::Instant::now() < deadline, "alpha never came up");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        id += 1;
    }

    let frame = json!({
        "jsonrpc": "2.0", "id": "watch", "method": "subscriptions/listen",
        "params": {"notifications": {"toolsListChanged": true}},
    });
    mcp.send(inline(frame)).await;
    let acknowledged = mcp.next().await;
    assert_eq!(
        acknowledged["method"], "notifications/subscriptions/acknowledged",
        "{acknowledged}"
    );

    let _beta_server = HttpServer::start(&beta, beta_port);
    let changed = mcp.next().await;
    assert_eq!(changed["method"], "notifications/tools/list_changed", "{changed}");
    assert_eq!(changed["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"], "watch");
    assert_eq!(listed(&mut mcp, 1000).await, ["alpha.read", "beta.status"]);
    proxy.shutdown().await;
}
