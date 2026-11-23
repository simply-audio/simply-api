# Changelog

What got added, per landing — dated with a clock and the jj change id
(one entry per landing, newest first).

## Unreleased

#### `simply-hooks` counts every event it gives up on · 2026-11-23 20:30 · syrzkxkp

- **Fixed, `simply-hooks`.** An event emitted into a sequence that was
  already given up on now counts as dropped in the target's `dropped`
  total, for the same reason `Hooks::deliveries` gives it ("not sent: an
  earlier event in its sequence was …"). Before, it showed in its
  deliveries but not in the totals, so a status could add up to fewer
  events than were emitted. Every emitted event now ends delivered,
  dropped or pending.

#### rmcp 3.11, and MCP `2026-07-28` served and tested · 2026-11-23 20:29 · llnxnqou, otvlsmyo

- **rmcp 3.11.0** in `simply-api`, `simply-relay` and `simply-relay-proxy`
  (requirement and lock). A consumer needs rmcp 3.11 or later in its own
  lock to take this tag.
- **`McpApiServer` now serves MCP `2026-07-28`**: no `initialize`, the
  lifecycle in each request's `_meta`, `server/discover`, `resultType` on
  every result, `ttlMs`/`cacheScope` on `tools/list`. Tests assert each of
  these on the wire. Clients on older revisions get the same replies as
  before, `2024-11-05` through `2025-11-25`, Lumina's rmcp 1.3 client included.
- `simply_api::mcp::MAX_PROTOCOL_VERSION` is the newest revision served. It
  is pinned, so a later rmcp that learns a newer revision does not start
  serving it untested.
- A tier change (`McpApiServer`), a switch flipped in the app (`simply-relay`)
  and a local server coming or going (`simply-relay-proxy`) now reach a
  `2026-07-28` client through `subscriptions/listen`. Before, it was told
  nothing. Older clients are still told on their session.

#### `simply-relay-proxy`: the MCP servers on your machine, served to Lumina as one device · 2026-11-21 15:13 · ytvkuwvx, oynnyyzo

- **New crate and program, `simply-relay-proxy`.** Run it on your own
  machine: it dials Lumina once with the relay's signed hello and serves
  the tools of every local MCP server you list in its config
  (`[servers.<name>]` with a streamable-HTTP `url` or a stdio `command`),
  named `<server>.<tool>`. Calls go to the server that owns the tool and
  come back as it answered.
- Servers come and go: one that starts, stops or changes its tools is
  announced to Lumina (`tools/list_changed`); a server that is down just
  has no tools and is retried with backoff (a stdio server is started
  again). A dropped relay is dialled again with backoff.
- `simply-relay-proxy pair CODE` swaps the code from `/devices` → Add
  device for the device key and writes it into the config file (mode
  0600); a re-keyed device's new key is written back the same way.
- Switches in `[consent]`, all off by default: `read_only` covers tools
  the server marks `readOnlyHint` (or its `read_only` list names); each
  server's other tools are under a switch named after it.
- **`simply-relay`.** `Builder::classify` classes the tools nobody declared
  up front, for a handler whose tools change while it runs. Declared tools
  behave as before.

## 0.3.0 — 2026-11-21

#### `simply-relay` signs its hello with the device key; `#[api(read_only)]` · 2026-11-21 12:04 · vkxsmmsr, mllxrnty

- **Breaking, `simply-relay`.** The relay no longer pairs or keeps tokens.
  The app links itself with `simply_hooks::device::link` (a one-time code
  from the "Add device" button in the owner's `/devices` view, or a
  member's `/<app> link`), keeps the
  key in its keychain, and gives it to the relay through
  `Builder::key(|| Some(DeviceKey { device_id, key }))`. Every hello is
  signed with it: `sha256=` HMAC over `<timestamp>.hello.<device_id>`
  (`wire::sign_hello`; test vector in the `wire` docs). When the owner has
  re-keyed the device, the welcome carries the new key and the relay hands
  it to `Builder::on_new_key` before it serves anything.
- `Relay::pair`, `PairError`, `Linked`, the `Unlink` frame and the token
  fields of `lumina.toml` are gone; the file keeps the switches only.
  `Relay::connect` dials now (after linking, or after an unlinked or
  stopped status); `Relay::disconnect` stops dialling. `Status` gains
  `NotLinked` and `Disconnected`, `Connected` names the `member`, and
  `Retrying` says which refusal it follows.
- Lumina's refusals: `unlinked` stops dialling; `clock`, `scope` and
  `disabled` keep the link and retry at the slowest pace; `limit` and any
  reason not known yet back off; `version` stops until the app is updated.
- The default endpoint is `wss://lumina.simply.audio/devices/v1`.
- Every tool the relay lists carries its class: `annotations.readOnlyHint`
  and `_meta["lumina/class"]` (`read_only` or the action class).
- **`simply-api`.** `#[api(read_only)]` marks an operation that only reads;
  `{TRAIT}_META.read_only_tools()` lists them, and
  `{TRAIT}_META.actions_in(tier)` lists a tier's other tools, so an app
  passes its read-only and action lists to the relay from the trait.
  `MethodMeta` gains `read_only`. An unmarked operation is not read-only.
- The workspace version is 0.3.0.
- `simply-api` and `simply-relay` need rmcp ≥ 3.4.0 and no longer use
  rmcp's deprecated `ServerInfo` (now `ServerConfig`), so a consumer on a
  newer rmcp 3.x builds them with `-D warnings` and gets no warnings
  (2026-11-21 13:54 · ymwroztt).

## 0.2.0 — 2026-11-12

#### The repo lives at `simply-audio/simply-api`; each crate has a README · 2026-11-12 21:42 · ppyksuwr, wsnkwunl

- The repository URL is https://github.com/simply-audio/simply-api; pin
  `tag = "v0.2.0"` there. Each published crate ships a README of its own.

#### `simply-hooks`: each event's own outcome · 2026-11-10 12:13 · uxnlvkmo

- `emit` and `emit_in` return the event's `EventId` (`None` when the event
  is refused at once: a bad name, data that isn't JSON, a body over the
  limit). `Hooks::deliveries(id)` says what became of that event at each
  target: pending (attempts, last failure, next try, or waiting on its
  sequence), delivered, or dropped with the reason (refused with the
  receiver's status and `{"error"}`, too old, pushed out of a full outbox,
  its target removed, or an earlier delivery in its sequence given up on,
  with that one's reason). A UI shows each event's own result instead of
  reading a target's totals, which a later refusal changes for every
  event.
- A delivery still in the outbox keeps its event id across a restart. The
  outcomes of the newest `max_pending` events that ended are kept while
  the app runs; one that ended before a restart reads as unknown (`None`).
- The outbox keeps each delivery's event id and last failure; outboxes
  written before read as they were.
- `wait_idle` no longer returns between an emit being taken in and the
  status showing it pending.

#### `simply-hooks`: deliveries in sequence · 2026-11-10 10:51 · pvxrtnsn

- `Hooks::emit_in(seq, event, data)` emits in a named sequence: to each
  target, a delivery is not tried before the earlier ones in its sequence
  are delivered, across retries and restarts; one given up on (refused,
  too old, pushed out of a full outbox) drops the rest of its sequence and
  anything emitted in it later. For an event that only makes sense after
  another (`jam.ended` after its `jam.shared`). Other deliveries never wait
  on a sequence, and `emit` behaves as before.
- The outbox keeps a delivery's sequence; outboxes written before read as
  they were.

#### `simply-hooks`: linked devices · 2026-11-10 09:49 · nnnksosu

- An app install can be **linked** to its member's account at a receiver
  (Lumina's `/<app> link`): `device::link(base, request, timeout)` swaps
  the one-time code the member was shown (read in any case, dashes
  dropped) for a device id and a key, at `POST /devices/v1/link`;
  `device::unlink` tells the receiver to forget it, signed by the device.
  A refusal says its status, the receiver's reason and its `Retry-After`.
- `Target::device(url, device_id, key)` signs deliveries with the device's
  key and adds `X-Hook-Device: <device id>` to every attempt, retries from
  the outbox included. Any target may carry headers of its own
  (`Target::header`); one with a line break is never sent.
- The `keychain` feature keeps a device's key in the macOS keychain
  (`keys::Keychain`); off macOS it keeps nothing and says so.
  `keys::MemoryKeys` is for tests.
- A delivery refused for good now says the receiver's reason in the
  status (`refused: HTTP 401 (device not linked)`), not only its code.
- The crate docs pin Lumina's device-signing test vector.

#### `simply-hooks`: signed outbound webhooks · 2026-11-09 10:15 · wnxonwuo, yzloxkmn, sqlzlqtw, lzmurqlqktsn

- New crate **`simply-hooks`**. An app calls `emit(event, data)` and it
  returns at once; a worker thread POSTs
  `{"event", "at", "data"}` to every target that wants the event. Each
  target has a URL, an optional secret and an optional event list
  (`jam.*` style prefixes allowed). A target without a secret
  (`Target::unsigned`, for a public intake) gets no `X-Signature` and no
  `X-Timestamp`. The app needs no async runtime.
- Every request carries the headers Lumina's hooks listener verifies and
  simply.audio sends: `X-Signature: sha256=<hex HMAC-SHA256(secret,
  "<timestamp>.<body>")>`, `X-Timestamp` (unix seconds), `X-Hook-Delivery`
  (a UUID v4) and `X-Hook-Event`. A retry keeps the delivery id and body
  and signs again. The crate docs hold a test vector other repos can pin,
  and `sign::verify` for receivers (several comma-separated signatures
  pass if one does).
- Retries: 10 s doubling to 10 min, for a day; a 10 s timeout. A `2xx`
  delivers; `408`, `425`, `429`, `5xx` and network errors retry; any other
  answer is refused for good. A `Retry-After` longer than the backoff is
  waited out (up to an hour). A body over 256 KiB, Lumina's limit, is
  refused at `emit`. `https://` only, except to localhost.
- Pending deliveries are kept in one owner-only JSON file in the app's data
  folder, so they survive a restart; it never holds a secret. Failures are
  logged with the event, the delivery id and the host: never the body.
- `status()` gives a settings screen what is pending, delivered and
  dropped, and the last error, per target. `set_targets` applies changed
  settings while the app runs.
- Nothing is added to the lockfile: the HTTP client is one POST over
  tokio and rustls, which the workspace already built.
- The receiver's whole answer (headers and body) is read, so a later
  follow-up to a bug report can pick up a session id from the reply and
  long-poll with the same signed POST. Nothing changes for apps yet.

#### `simply-relay`: an app can link itself to Lumina · 2026-11-09 08:35 · vmlmllrk..ouppwpny

- New crate **`simply-relay`**. An app (tussen, flux) opens a WebSocket out to
  Lumina and serves its existing MCP tools there, so Lumina can check a
  member's setup when they report a bug. Nothing on the member's side
  listens, so NAT and firewalls don't matter. The crate does not depend on
  `simply-api`, so an app can adopt it before it migrates to `simply-api`.
- The member links an app by pasting the one-time code `/support link`
  shows. The app keeps one file, `lumina.toml`, readable by the member
  only (0600). It holds the device token, which Lumina replaces on every
  connect, and the consent switches.
- Everything is **off by default**. One switch allows read-only diagnostics.
  Each action class (jam sessions, later) has its own switch. A tool that
  isn't switched on is neither listed nor run: the app refuses it before its
  own code sees it. Each app lists its read-only tools by hand.
- Every call Lumina makes, allowed or refused, is written to
  `lumina-calls.log` beside the token file, with the reason Lumina gave.
  Argument values are not written.
- Every app gets the same diagnostics: `lumina_about`, `lumina_config`
  (secrets blanked by the app's redactor and then by key name, home
  directory shown as `~`), `lumina_errors`, `lumina_log` and
  `lumina_report`. A tracing layer, `simply_relay::log_layer()`, keeps the
  log lines they read.
- Apps compile it in behind their own optional `lumina` feature.
- A test shows Lumina's rmcp 1.3 client working with the apps' rmcp 3.x
  servers over this connection, protocol `2025-06-18`. It lives in the
  dev-only crate `relay-compat`, which is never published.

## 0.1.1 — 2026-10-23

#### MIT only (`LICENSE-MIT-ONLY`) · 2026-10-23 · kxrnwllw

- Licence is **MIT**, matching tussen; no longer `MIT OR Apache-2.0`.
  `LICENSE-MIT` is now `LICENSE`, `LICENSE-APACHE` is gone, every crate
  declares `license = "MIT"`. `v0.1.0` stays as published; depend on `v0.1.1`.
- Public-repo cleanup: no local machine paths in the docs, the internal
  sense comparison note removed, `cargo doc` warning-free (I-001).

## 0.1.0 — 2026-10-23

#### Published as `poucet/simply-api` (`API-PUBLISH`) · 2026-10-23 13:58 · ywtzoswx

- The repo is named `simply-api`, after its main crate. It was `simply-kit`
  while the name was being decided. It is public at
  https://github.com/poucet/simply-api, with `main` and `v0.1.0` pushed.
- Consumers depend on it like this:
  `simply-api = { git = "https://github.com/poucet/simply-api", tag = "v0.1.0" }`.

#### Ready to publish: CI, licence files, Cargo metadata (`KIT-PUBLISH-PREP`) · 2026-10-23 13:57 · rqqsmoty

- CI runs `cargo fmt --check` on Linux, and `cargo test` and `cargo clippy -D
  warnings` (both `--locked`) on Linux and macOS. The workflow is modelled on
  flux's `ci.yml`.
- `LICENSE-MIT` and `LICENSE-APACHE` are added. Every crate now declares
  `repository`, `authors` and `readme`.
- The README has a "Using it" section (a git dependency by tag, plus a minimal
  service checked to compile) and the dual-licence statement.
- Crate docs no longer name a flux-only file path.

#### The API derive and the history tree get their own repo (`KIT-EXTRACT`) · 2026-10-23 13:53 · orlnwpsz

- `simply-api`, `simply-api-macros`, `simply-history` and `simply-op-macros`
  are extracted from flux at `bae34fcf` with the same code and the same
  dependency versions. The versions are reset to 0.1.0. Licence:
  `MIT OR Apache-2.0`.
- Nothing changes yet for flux, tussen or sense: none of them depends on this
  repo yet.
