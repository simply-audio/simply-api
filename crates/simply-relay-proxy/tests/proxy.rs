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
    common::relay::until(proxy.relay(), |s| matches!(s, Status::Connected { .. })).await;
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

    // Not linked: the proxy waits rather than dial.
    let unlinked = dir.path().join("unlinked.toml");
    std::fs::write(&unlinked, format!("relay = \"{}\"\n", lumina.url)).unwrap();
    let proxy = simply_relay_proxy::Proxy::start(&unlinked, common::options()).unwrap();
    common::relay::until(proxy.relay(), |s| matches!(s, Status::NotLinked)).await;
    assert!(lumina.quiet_for(std::time::Duration::from_millis(200)).await);
    proxy.shutdown().await;
}
