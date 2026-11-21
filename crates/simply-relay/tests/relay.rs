//! The app side end to end, against a scripted Lumina on loopback.

mod common;

use std::time::Duration;

use common::{relay, signed_with, until, FakeLumina, Keychain, Studio, DEVICE, KEY, NEW_KEY};
use serde_json::{json, Value};
use simply_relay::wire::RefusedReason;
use simply_relay::{Status, Verdict};

fn stored(dir: &std::path::Path) -> toml::Table {
    toml::from_str(&std::fs::read_to_string(dir.join("lumina.toml")).unwrap()).unwrap()
}

#[cfg(unix)]
fn mode(dir: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir.join("lumina.toml")).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn a_linked_app_says_a_signed_hello_and_serves() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let keychain = Keychain::linked();
    let app = relay(dir.path(), &lumina, &keychain).spawn(Studio::default).unwrap();

    let (hello, conn) = lumina.accept().await;
    assert_eq!((hello.v, hello.app.as_str(), hello.version.as_str()), (1, "studio", "1.2.3"));
    assert_eq!((hello.label.as_str(), hello.protocol.as_str()), ("test-mac", "mcp"));
    assert_eq!(hello.device_id, DEVICE);
    assert!(signed_with(&hello, KEY), "signed now, with the device key: {hello:?}");
    assert!(!hello.consent.read_only, "everything is off by default");
    assert_eq!(hello.consent.actions.get("session"), Some(&false));

    let mcp = conn.welcome(Some("chris")).await;
    let status = until(&app, |s| matches!(s, Status::Connected { .. })).await;
    assert_eq!(
        status,
        Status::Connected { device_id: DEVICE.into(), member: Some("chris".into()) }
    );
    // With every switch off, Lumina sees nothing.
    assert!(mcp.tool_names().await.is_empty());

    // The relay keeps the switches only; the key stays in the keychain.
    app.set_read_only(true).unwrap();
    let file = std::fs::read_to_string(dir.path().join("lumina.toml")).unwrap();
    assert!(!file.contains(KEY) && !file.contains(DEVICE), "{file}");
    #[cfg(unix)]
    assert_eq!(mode(dir.path()), 0o600);

    // Lumina goes away; the app dials again, freshly signed with the same key.
    mcp.client.cancel().await.unwrap();
    let (hello, conn) = lumina.accept().await;
    assert!(signed_with(&hello, KEY));
    assert!(hello.consent.read_only, "the hello reports the switches as they are");
    drop(conn);
    until(&app, |s| matches!(s, Status::Retrying { reason: None, .. })).await;
    let (hello, _conn) = lumina.accept().await;
    assert!(signed_with(&hello, KEY), "a failed connect is retried");
    app.shutdown().await;
}

#[tokio::test]
async fn an_unlinked_app_waits_until_told_to_connect() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let keychain = Keychain::default();
    let app = relay(dir.path(), &lumina, &keychain).spawn(Studio::default).unwrap();
    until(&app, |s| *s == Status::NotLinked).await;
    assert!(lumina.quiet_for(Duration::from_millis(200)).await, "nothing is dialled");

    // The app links over HTTP, keeps the key, and tells the relay.
    keychain.set(KEY);
    app.connect();
    let (hello, conn) = lumina.accept().await;
    assert!(signed_with(&hello, KEY));
    let _mcp = conn.welcome(Some("chris")).await;
    until(&app, |s| matches!(s, Status::Connected { .. })).await;

    // Disconnect: the socket closes and nothing is dialled until connect.
    app.disconnect().await;
    assert_eq!(app.status(), Status::Disconnected);
    assert!(lumina.quiet_for(Duration::from_millis(200)).await, "nothing is dialled");
    app.connect();
    let (hello, _conn) = lumina.accept().await;
    assert!(signed_with(&hello, KEY));
    app.shutdown().await;
}

#[tokio::test]
async fn a_re_keyed_device_stores_its_new_key_before_serving() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let keychain = Keychain::linked();
    let app = relay(dir.path(), &lumina, &keychain).spawn(Studio::default).unwrap();

    // The keychain refuses the new key: nothing is served, and the next
    // hello is still signed with the old key.
    *keychain.refuse_new.lock().unwrap() = true;
    let (hello, conn) = lumina.accept().await;
    assert!(signed_with(&hello, KEY));
    assert!(conn.welcome_with(Some("chris"), Some(NEW_KEY)).await.is_err(), "not served");
    let status = until(&app, |s| matches!(s, Status::Retrying { .. })).await;
    assert!(format!("{status:?}").contains("the keychain is locked"), "{status:?}");
    assert_eq!(keychain.key().as_deref(), Some(KEY));

    // Stored: by the time MCP answers, the keychain holds the new key, and
    // the next dial signs with it.
    *keychain.refuse_new.lock().unwrap() = false;
    let (hello, conn) = lumina.accept().await;
    assert!(signed_with(&hello, KEY));
    let mcp = conn.welcome_with(Some("chris"), Some(NEW_KEY)).await.expect("served");
    assert_eq!(keychain.key().as_deref(), Some(NEW_KEY));
    mcp.client.cancel().await.unwrap();
    let (hello, _conn) = lumina.accept().await;
    assert!(signed_with(&hello, NEW_KEY), "{hello:?}");
    assert!(!signed_with(&hello, KEY));
    app.shutdown().await;
}

#[tokio::test]
async fn each_refusal_leads_where_the_wire_says() {
    use RefusedReason::*;
    // (reason, slowest pace?) for the reasons that keep dialling. The test
    // backoff runs 20 ms to 200 ms, jittered ±25 %.
    for (reason, slowest) in
        [(Clock, true), (Scope, true), (Disabled, true), (Limit, false), (Other, false)]
    {
        let dir = tempfile::tempdir().unwrap();
        let lumina = FakeLumina::start().await;
        let app = relay(dir.path(), &lumina, &Keychain::linked()).spawn(Studio::default).unwrap();
        let (_, conn) = lumina.accept().await;
        conn.refuse(reason.clone(), "no").await;
        let status = until(&app, |s| matches!(s, Status::Retrying { .. })).await;
        let Status::Retrying { after, reason: seen, .. } = status else { unreachable!() };
        assert_eq!(seen, Some(reason.clone()));
        if slowest {
            assert!(after >= Duration::from_millis(150), "{reason:?}: {after:?}");
        } else {
            assert!(after <= Duration::from_millis(25), "{reason:?}: {after:?}");
        }
        let (hello, _conn) = lumina.accept().await;
        assert!(signed_with(&hello, KEY), "{reason:?}: the link is kept");
        app.shutdown().await;
    }

    // The reasons that stop dialling until the app says connect.
    for reason in [Unlinked, Version] {
        let dir = tempfile::tempdir().unwrap();
        let lumina = FakeLumina::start().await;
        let app = relay(dir.path(), &lumina, &Keychain::linked()).spawn(Studio::default).unwrap();
        let (_, conn) = lumina.accept().await;
        conn.refuse(reason.clone(), "This device is no longer linked.").await;
        let message = "This device is no longer linked.".to_owned();
        let expected = match reason {
            Unlinked => Status::Unlinked { message },
            _ => Status::Stopped { message },
        };
        until(&app, |s| *s == expected).await;
        assert!(lumina.quiet_for(Duration::from_millis(300)).await, "{reason:?}: no dialling");
        app.connect();
        let (hello, _conn) = lumina.accept().await;
        assert!(signed_with(&hello, KEY), "{reason:?}");
        app.shutdown().await;
    }
}

/// Connect and return a connected Lumina client.
async fn paired(
    dir: &std::path::Path,
    lumina: &FakeLumina,
    builder: impl FnOnce(simply_relay::Builder) -> simply_relay::Builder,
    studio: &Studio,
) -> (simply_relay::Relay, common::Mcp) {
    let studio = studio.clone();
    let app =
        builder(relay(dir, lumina, &Keychain::linked())).spawn(move || studio.clone()).unwrap();
    let (_, conn) = lumina.accept().await;
    let mcp = conn.welcome(None).await;
    until(&app, |s| matches!(s, Status::Connected { .. })).await;
    (app, mcp)
}

#[tokio::test]
async fn a_tool_the_switches_do_not_allow_is_refused_before_the_app_sees_it() {
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let studio = Studio::default();
    let (app, mut mcp) = paired(dir.path(), &lumina, |b| b, &studio).await;

    let refused = mcp.call("levels", json!({}), "bug studio-0042").await.unwrap_err();
    assert_eq!(refused.code.0, -32602);
    assert_eq!(refused.data, Some(json!({"refused": "switched_off", "class": "read_only"})));
    assert!(
        refused.message.contains("diagnostics are switched off in studio"),
        "{}",
        refused.message
    );
    assert!(studio.reached().is_empty(), "the handler never saw it");

    // Diagnostics on: Lumina is told to re-list, and read-only tools answer.
    app.set_read_only(true).unwrap();
    tokio::time::timeout(common::WAIT, mcp.list_changed.recv()).await.unwrap().unwrap();
    // Every listed tool carries its class, the diagnostics included.
    let tools = mcp.tools().await;
    for tool in &tools {
        let read_only = tool.annotations.as_ref().and_then(|a| a.read_only_hint);
        let class = tool.meta.as_ref().and_then(|m| m.0.get("lumina/class").cloned());
        assert_eq!((read_only, class), (Some(true), Some(json!("read_only"))), "{}", tool.name);
    }
    let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "echo",
            "levels",
            "lumina_about",
            "lumina_config",
            "lumina_errors",
            "lumina_log",
            "lumina_report"
        ]
    );
    let levels = mcp.call("levels", json!({}), "bug studio-0042").await.unwrap();
    assert_eq!(levels.structured_content, Some(json!({"rate": 48000, "peaks": [-6.0]})));

    // An action stays off under the diagnostics switch; an undeclared tool
    // is never offered, whatever is on.
    let action = mcp.call("open_session", json!({}), "jam #jam-chris").await.unwrap_err();
    assert_eq!(action.data, Some(json!({"refused": "switched_off", "class": "session"})));
    let wipe = mcp.call("wipe", json!({}), "?").await.unwrap_err();
    assert_eq!(wipe.data, Some(json!({"refused": "not_exposed", "class": null})));

    app.set_action("session", true).unwrap();
    tokio::time::timeout(common::WAIT, mcp.list_changed.recv()).await.unwrap().unwrap();
    let tools = mcp.tools().await;
    let open = tools.iter().find(|t| t.name == "open_session").expect("listed");
    assert_eq!(open.annotations.as_ref().and_then(|a| a.read_only_hint), Some(false));
    assert_eq!(
        open.meta.as_ref().and_then(|m| m.0.get("lumina/class").cloned()),
        Some(json!("session"))
    );
    assert!(!tools.iter().any(|t| t.name == "wipe"), "undeclared: never listed");
    mcp.call("open_session", json!({}), "jam #jam-chris").await.unwrap();
    assert!(mcp.call("wipe", json!({}), "?").await.is_err());
    assert_eq!(studio.reached(), ["levels", "open_session"]);
    assert!(app.set_action("nonsense", true).is_err());

    // The member's log has every call, with Lumina's stated purpose.
    let calls = app.recent_calls(50);
    let summary: Vec<(&str, &str, Verdict, Option<&str>)> = calls
        .iter()
        .map(|c| (c.tool.as_str(), c.class.as_str(), c.verdict, c.purpose.as_deref()))
        .collect();
    assert_eq!(
        summary,
        [
            ("levels", "read_only", Verdict::Refused, Some("bug studio-0042")),
            ("levels", "read_only", Verdict::Allowed, Some("bug studio-0042")),
            ("open_session", "session", Verdict::Refused, Some("jam #jam-chris")),
            ("wipe", "none", Verdict::Refused, Some("?")),
            ("open_session", "session", Verdict::Allowed, Some("jam #jam-chris")),
            ("wipe", "none", Verdict::Refused, Some("?")),
        ]
    );
    assert!(
        !std::fs::read_to_string(app.call_log_path()).unwrap().contains("text"),
        "no arguments"
    );

    // The switches survive a restart.
    app.shutdown().await;
    let file = stored(dir.path());
    assert_eq!(file["consent"]["read_only"].as_bool(), Some(true));
    assert_eq!(file["consent"]["actions"]["session"].as_bool(), Some(true));
}

#[tokio::test]
async fn diagnostics_redact_the_config_and_report_everything() {
    use tracing_subscriber::layer::SubscriberExt;
    let subscriber = tracing_subscriber::registry().with(simply_relay::log_layer());
    let _guard = tracing::subscriber::set_default(subscriber);

    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/someone".into());
    let dir = tempfile::tempdir().unwrap();
    let lumina = FakeLumina::start().await;
    let studio = Studio::default();
    let config_home = home.clone();
    let (app, mcp) = paired(
        dir.path(),
        &lumina,
        move |b| {
            b.build("abc123")
                .config(move || {
                    json!({
                        "audio": {"rate": 48000, "device": "MOTU M4", "keyboard": "on"},
                        "api_key": "sk-live-1",
                        "cloud": {"accessToken": "tok", "password": "hunter2", "author": "chris"},
                        "secrets": {"github": "ghp_x", "list": ["a", "b"]},
                        "projects_dir": format!("{config_home}/Music/projects"),
                        "studio_name": "Chris's attic",
                    })
                })
                .redactor(|config| {
                    config["studio_name"] = Value::String("[hidden by the app]".into());
                })
        },
        &studio,
    )
    .await;
    app.set_read_only(true).unwrap();

    let config =
        mcp.call("lumina_config", json!({}), "bug").await.unwrap().structured_content.unwrap();
    assert_eq!(
        config["config"],
        json!({
            "audio": {"rate": 48000, "device": "MOTU M4", "keyboard": "on"},
            "api_key": "[redacted]",
            "cloud": {"accessToken": "[redacted]", "password": "[redacted]", "author": "chris"},
            "secrets": {"github": "[redacted]", "list": ["[redacted]", "[redacted]"]},
            "projects_dir": "~/Music/projects",
            "studio_name": "[hidden by the app]",
        })
    );

    let marker = "relay-diag-marker-7f3a";
    tracing::info!("opened {home}/Music/{marker}.wav");
    tracing::error!("{marker}: device lost");
    let log = mcp
        .call("lumina_log", json!({"lines": 500}), "bug")
        .await
        .unwrap()
        .structured_content
        .unwrap();
    let lines = log["lines"].as_array().unwrap();
    let opened =
        lines.iter().find(|l| l["message"].as_str().unwrap().contains(marker)).expect("logged");
    assert_eq!(opened["message"], format!("opened ~/Music/{marker}.wav"));
    let errors =
        mcp.call("lumina_errors", json!({}), "bug").await.unwrap().structured_content.unwrap();
    assert!(errors["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["level"] == "ERROR" && l["message"].as_str().unwrap().contains(marker)));

    let about =
        mcp.call("lumina_about", json!({}), "bug").await.unwrap().structured_content.unwrap();
    assert_eq!((about["app"].as_str(), about["version"].as_str()), (Some("studio"), Some("1.2.3")));
    assert_eq!(about["build"], "abc123");
    assert_eq!(about["os"], std::env::consts::OS);

    // The report runs `levels` (no argument) but not `echo` (one required).
    let report = mcp
        .call("lumina_report", json!({}), "bug studio-7")
        .await
        .unwrap()
        .structured_content
        .unwrap();
    assert_eq!(report["checks"], json!({"levels": {"rate": 48000, "peaks": [-6.0]}}));
    assert_eq!(report["config"]["api_key"], "[redacted]");
    assert_eq!(report["about"]["app"], "studio");
    assert!(report["log"].is_array() && report["errors"].is_array());
    assert_eq!(studio.reached(), ["levels"]);
    let last = app.recent_calls(2);
    assert_eq!(
        last.iter().map(|c| (c.tool.as_str(), c.purpose.as_deref())).collect::<Vec<_>>(),
        [("lumina_report", Some("bug studio-7")), ("levels", Some("bug studio-7"))]
    );
    app.shutdown().await;
}
