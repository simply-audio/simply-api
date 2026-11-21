# simply-relay-proxy

Serve the MCP servers on your own machine to Lumina over one relay connection: their tools, together, as one device.

The proxy is a small program you run on your computer. It dials Lumina once (`wss://lumina.simply.audio/devices/v1`, the signed hello of [`simply-relay`](../simply-relay)), and serves one MCP server there whose tools are the tools of every local MCP server you list, named `<server>.<tool>`. Lumina shows them as `device:<machine>.<server>.<tool>`. To each local server the proxy is an ordinary MCP client, over streamable HTTP or stdio, so the apps need nothing new.

- A call goes to the server that owns the tool; its answer (content, structured content, errors) comes back as the server gave it.
- When a local server starts, stops or changes its tools, the proxy tells Lumina (`notifications/tools/list_changed`). A server that is down simply has no tools; the proxy tries it again with backoff, and starts a stdio server again.
- If the connection to Lumina drops, the proxy dials again with backoff.
- Nothing listens on your machine for Lumina, so NAT and firewalls don't matter.

## Install

```sh
cargo install --git https://github.com/simply-audio/simply-api simply-relay-proxy
```

(add `--tag vX.Y.Z` for a release).

## Pair

In Discord, the owner opens `/devices`, presses **Add device**, names the machine (`studio`) and gets a one-time code. Then, on the machine:

```sh
simply-relay-proxy pair ABCD-EFGH-JKMN
```

That swaps the code for the device's key at Lumina and writes it into the config file, which the proxy keeps readable by you only. Lumina never sees the key again: every hello is signed with it. When the owner re-keys the device in `/devices`, the next connection hands over the new key and the proxy writes it into the file.

## Configure

The config file is `simply-relay-proxy/config.toml` in your config directory (`~/Library/Application Support/` on macOS, `~/.config/` on Linux, `%APPDATA%` on Windows); `--config FILE` points elsewhere. Pair first or write it first, either order works.

```toml
# relay = "wss://lumina.simply.audio/devices/v1"   # the default
# label = "studio-mac"                             # what Lumina shows; default: the host name

# What Lumina may call. Everything is off until you turn it on.
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

[device]   # written by `pair`
id = "…"
key = "…"
```

A tool is **read-only** when its server marks it `readOnlyHint`, or when the server's `read_only` list names it; it is then under the `read_only` switch. Every other tool of a server is under that server's own switch in `actions`. A server name may not contain a dot or be `read_only`. Local servers are reached over plain HTTP, as they listen on the machine itself.

Beside the config file the proxy keeps `lumina.toml` (the switches as last applied, which the relay reports in its hello) and the call log, one line per call Lumina made: time, tool, class, verdict and Lumina's stated purpose, never the arguments. The standard diagnostics (`lumina_about`, `lumina_log`, …) are served too, under `read_only`.

## Run

```sh
simply-relay-proxy                 # or: simply-relay-proxy --config ~/proxy.toml
RUST_LOG=debug simply-relay-proxy  # more detail
```

It logs when a server comes up or goes down and what the relay is doing (connected, retrying and why; a `clock` refusal means the computer's clock is off). Stop it with Ctrl-C. The config is read at start, apart from the key, which is read before every dial; restart the proxy after editing servers or switches.

## Licence

MIT — see [LICENSE](../../LICENSE).
