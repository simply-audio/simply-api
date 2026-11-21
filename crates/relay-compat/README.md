# relay-compat

Dev-only: proves Lumina's rmcp 1.3 client and an app's rmcp 3.x server talk over the relay's WebSocket.

Nothing but tests. It exists so rmcp 1.3 (Lumina's) and rmcp 3.x (the apps') can meet in one test binary without either reaching a published crate. `tests/spike.rs` drives both over a real WebSocket on loopback: a 1.3 client against a 3.x `McpApiServer`, and the whole relay (signed hello, gate, diagnostics, the class stamped on each tool) against a 1.3 client playing Lumina's listener.

This crate is never published and has no consumers outside its own tests.

## Depending on it

You don't. It is dev-only infrastructure for this repo.

## Licence

MIT — see [LICENSE](../../LICENSE).
