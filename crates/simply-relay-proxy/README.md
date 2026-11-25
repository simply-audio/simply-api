# simply-relay-proxy

Serve the MCP servers on your own machine to one or more Lumina Discord servers.

The proxy is a small program you run on your computer. It keeps one client per local MCP server (over streamable HTTP or stdio) and opens one [`simply-relay`](../simply-relay) WebSocket connection (`wss://lumina.simply.audio/devices/v1`, signed hello) per linked device. Each linked server sees the same union of local tools, named `<server>.<tool>` (Lumina shows them as `device:<machine>.<server>.<tool>`), behind its own device key and consent switches. To each local server the proxy is an ordinary MCP client, so the apps need nothing new.

- A call goes to the server that owns the tool; its answer (content, structured content, errors) comes back as the server gave it.
- When a local server starts, stops or changes its tools, the proxy tells every connected Lumina (`notifications/tools/list_changed`). A server that is down simply has no tools; the proxy tries it again with backoff, and starts a stdio server again.
- If a connection to Lumina drops, the proxy dials again with backoff.
- Nothing listens on your machine for Lumina, so NAT and firewalls don't matter.

## Install

```sh
cargo install --git https://github.com/simply-audio/simply-api simply-relay-proxy
```

(add `--tag vX.Y.Z` for a release).

## Pair, list, and unpair

In Discord, the owner opens `/devices`, presses **Add device**, names the machine (`studio`) and gets a one-time code. Then, on the machine:

```sh
simply-relay-proxy pair ABCD-EFGH-JKMN
```

That swaps the code for a device key at Lumina and appends a `[[devices]]` entry to the config file, which the proxy keeps readable by you only. Run `pair` again with a code from another Discord server to link the same proxy process to multiple servers, each with its own individually revocable key. Lumina never sees a key again: every hello is signed with it. When an owner re-keys a device in `/devices`, the next connection hands over the new key and the proxy updates only that device's entry in place.

List all linked devices (their id, server, member, and any consent overrides):

```sh
simply-relay-proxy devices
```

Remove a linked device by its id or server name (notifying Lumina and removing it from `config.toml`; if a server name matches more than one device, pass the device id):

```sh
simply-relay-proxy unpair "Studio Discord"
simply-relay-proxy unpair 0123456789abcdef0123456789abcdef
```

## Configure

The config file is `simply-relay-proxy/config.toml` in your config directory (`~/Library/Application Support/` on macOS, `~/.config/` on Linux, `%APPDATA%` on Windows); `--config FILE` points elsewhere. Pair first or write it first, either order works.

```toml
# relay = "wss://lumina.simply.audio/devices/v1"   # the default
# label = "studio-mac"                             # what Lumina shows; default: the host name

# What Lumina may call by default across every linked device.
# Everything is off until you turn it on.
[consent]
read_only = true                         # tools that only read, from every server
actions = { tussen = true, flux = false } # every other tool, one switch per server

# tussen's MCP endpoint ([api] mcp_port in tussen.toml, 9097 by default)
[servers.tussen]
url = "http://127.0.0.1:9097/mcp"

# flux's (flux-gui or flux-mcp; [mcp] port in flux's config.toml, 3927 by default)
[servers.flux]
url = "http://127.0.0.1:3927/mcp"
read_only = ["get_graph", "list_modules", "help"]  # read-only even if flux doesn't say so

# any stdio MCP server: started by the proxy, restarted if it exits
[servers.files]
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/Users/me/Music"]

[[devices]]   # written by `pair`
id = "…"
key = "…"
server = "Studio Discord"
member = "chris"

[[devices]]   # a second server, with a per-device consent override
id = "…"
key = "…"
server = "Band Discord"
member = "chris"
[devices.consent]
actions = { tussen = false }   # inherits read_only = true from [consent]
```

A tool is **read-only** when its server marks it `readOnlyHint`, or when the server's `read_only` list names it; it is then under the `read_only` switch. Every other tool of a server is under that server's own switch in `actions`. A per-device `[devices.consent]` table overlays `read_only` and/or individual `actions` entries onto top-level `[consent]`. Existing configs with a single `[device]` table continue to work alongside `[[devices]]`. A server name may not contain a dot or be `read_only`. Local servers are reached over plain HTTP, as they listen on the machine itself.

Beside the config file the proxy keeps `devices/<id>/lumina.toml` (each device's switches as last applied, which its relay reports in its hello) and each device's call log, one line per call Lumina made: time, tool, class, verdict and Lumina's stated purpose, never the arguments. The standard diagnostics (`lumina_about`, `lumina_log`, …) are served too, under `read_only`.

## Run

```sh
simply-relay-proxy                 # or: simply-relay-proxy --config ~/proxy.toml
RUST_LOG=debug simply-relay-proxy  # more detail
```

It logs when a server comes up or goes down and what each device's relay is doing, tagged with its server name or short id (connected, retrying and why; a `clock` refusal means the computer's clock is off). Stop it with Ctrl-C. The config is read at start, apart from device keys, which are read before every dial; restart the proxy after editing servers, switches, or pairing a new device.

## Licence

MIT — see [LICENSE](../../LICENSE).
