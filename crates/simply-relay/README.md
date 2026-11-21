# simply-relay

An app dials out to Lumina over a WebSocket and serves its MCP tools there, gated by the member's consent.

The app opens one WebSocket to Lumina (`wss://lumina.simply.audio/devices/v1`), proves who it is in the first frame, a hello signed with its device key ([`wire`]), and then serves any rmcp 3.x [`ServerHandler`] over the socket, one JSON-RPC message per text frame ([`frames`]), with Lumina as the MCP client. Nothing on the member's side listens, so NAT and firewalls don't matter.

Between Lumina and the app's handler sits the [`gate`]: every tool is off until the member switches its class on, and a refused call never reaches the handler. Every listed tool carries its class (`annotations.readOnlyHint` and `_meta["lumina/class"]`). Every call is written to the member's own call log ([`calls`]). The standard diagnostics ([`diag`]) are served beside the app's tools.

## Linking

The relay does not link. The app links itself over HTTP with `simply_hooks::device::link` and keeps the device id and key in its keychain (`simply_hooks::keys`). The code it swaps comes from Discord: the "Add device" button in the owner's `/devices` view makes a device whose tools Lumina may offer and gives a one-time code; a member's own `/<app> link` gives one too, for a link Lumina never offers to a model. The app sends the code to `/devices/v1/link`; the answer is the key's only appearance. Unlinking is `simply_hooks::device::unlink`.

The relay asks the app for the key before every dial, and signs the hello with it. Keys change only when the owner re-keys the device: the next welcome carries the new key, and the relay hands it to the app before it serves anything.

```rust,ignore
#[cfg(feature = "lumina")]
let relay = simply_relay::Relay::builder("tussen", env!("CARGO_PKG_VERSION"), config_dir)
    .key(move || {
        let device_id = settings.device_id()?;
        let key = keychain.get(&device_id).ok().flatten()?;
        Some(simply_relay::DeviceKey { device_id, key })
    })
    .on_new_key(move |device| keychain.set(&device.device_id, &device.key))
    // read-only tools from the trait's `#[api(read_only)]` marks
    .read_only(TUSSEN_API_META.read_only_tools())
    .action("perform", TUSSEN_API_META.actions_in("perform"))
    .config(move || serde_json::to_value(hub.config()).unwrap_or_default())
    .redactor(|config| { config["studio"]["owner_email"] = "[redacted]".into(); })
    .spawn(move || McpApiServer::new(api.clone(), tussen_api_tool_router::<dyn TussenApi>()))?;
// once, where the app builds its tracing subscriber:
#[cfg(feature = "lumina")]
let subscriber = subscriber.with(simply_relay::log_layer());
```

The settings panel then calls [`Relay::set_read_only`] / [`Relay::set_action`] for the switches, [`Relay::recent_calls`] for the log, [`Relay::connect`] once the app has linked (or after an unlinked or stopped status) and [`Relay::disconnect`] to stop dialling. [`Relay::watch_status`] says what to show: connected and to whom, retrying and why (a `clock` refusal means the computer's clock is off), unlinked (offer to link again), stopped (update the app).

## Adopting it

An app compiles the relay in behind its own optional feature, so a build without it carries none of this:

```toml
[features]
lumina = ["dep:simply-relay"]
[dependencies]
simply-relay = { git = "https://github.com/simply-audio/simply-api", tag = "v0.3.0", optional = true }
```

## Consumers

No app depends on it yet; Lumina is the server it dials. Deliberately independent of `simply-api`: it serves any rmcp 3.x `ServerHandler`, so an app can adopt it before it uses simply-api. It depends on `simply-hooks` for the device link and its signing.

## Licence

MIT — see [LICENSE](../../LICENSE).
