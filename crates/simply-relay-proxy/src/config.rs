//! The config file: where Lumina is, the device's key, the switches and the
//! local MCP servers.
//!
//! ```toml
//! # relay = "wss://lumina.simply.audio/devices/v1"   # the default
//! # label = "studio-mac"                             # default: the host name
//!
//! [consent]
//! read_only = true                 # tools that only read, from every server
//! actions = { tussen = true }      # every other tool, one switch per server
//!
//! [servers.tussen]
//! url = "http://127.0.0.1:7878/mcp"
//!
//! [servers.notes]
//! command = ["notes-mcp", "--stdio"]
//! read_only = ["search"]           # read-only even if the server doesn't say
//!
//! [device]                         # written by `simply-relay-proxy pair`
//! id = "0123456789abcdef0123456789abcdef"
//! key = "…"
//! ```
//!
//! The file holds the device key, so the proxy writes it readable by its
//! owner only.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use simply_relay::{Consent, DeviceKey, Secret};

/// The app name the proxy says hello and links as.
pub const APP: &str = "simply-relay-proxy";

/// The config file's name in [`default_dir`].
pub const FILE_NAME: &str = "config.toml";

/// The whole file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Lumina's relay endpoint.
    #[serde(default = "default_relay")]
    pub relay: String,
    /// What Lumina shows this machine as. Default: the host name.
    #[serde(default)]
    pub label: Option<String>,
    /// The switches; all off by default.
    #[serde(default)]
    pub consent: Consent,
    /// The local MCP servers, by name.
    #[serde(default)]
    pub servers: BTreeMap<String, Server>,
    /// The device's id and key, written by `pair`.
    #[serde(default)]
    pub device: Option<Device>,
}

fn default_relay() -> String {
    simply_relay::wire::DEFAULT_ENDPOINT.to_owned()
}

/// One local MCP server, reached at `url` or by running `command`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// A streamable-HTTP endpoint (`http://127.0.0.1:7878/mcp`).
    #[serde(default)]
    pub url: Option<String>,
    /// A program and its arguments, spoken to over stdio.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Tools to treat as read-only whatever the server's `readOnlyHint`.
    #[serde(default)]
    pub read_only: BTreeSet<String>,
}

/// How to reach a local server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Http(String),
    Stdio(Vec<String>),
}

impl Server {
    /// The server's transport, or why the entry is wrong.
    pub fn transport(&self) -> Result<Transport, String> {
        match (&self.url, &self.command) {
            (Some(url), None) => Ok(Transport::Http(url.clone())),
            (None, Some(command)) if !command.is_empty() => Ok(Transport::Stdio(command.clone())),
            (None, Some(_)) => Err("`command` is empty".into()),
            _ => Err("needs exactly one of `url` and `command`".into()),
        }
    }
}

/// The `[device]` table.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub id: String,
    pub key: String,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Device {
    pub fn key(&self) -> DeviceKey {
        DeviceKey { device_id: self.id.clone(), key: Secret::new(&self.key) }
    }
}

fn invalid(path: &Path, message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{}: {message}", path.display()))
}

impl Config {
    /// Read and check the file. A missing file is an empty config.
    pub fn load(path: &Path) -> io::Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let config: Config = toml::from_str(&text).map_err(|e| invalid(path, e))?;
        config.check().map_err(|e| invalid(path, e))?;
        Ok(config)
    }

    /// Server names become the first part of every tool name and the name
    /// of their action switch.
    fn check(&self) -> Result<(), String> {
        for (name, server) in &self.servers {
            if name.is_empty() || name.contains('.') || name == "read_only" {
                return Err(format!(
                    "server name {name:?}: it may not be empty, contain a dot or be `read_only`"
                ));
            }
            server.transport().map_err(|e| format!("[servers.{name}] {e}"))?;
        }
        Ok(())
    }
}

/// Write `device` into the `[device]` table of the file at `path`, keeping
/// everything else in it as it was; the file ends up readable by its owner
/// only.
pub fn store_device(path: &Path, device: &DeviceKey) -> io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| invalid(path, e))?;
    let mut table = toml_edit::Table::new();
    table["id"] = toml_edit::value(device.device_id.clone());
    table["key"] = toml_edit::value(device.key.expose());
    doc["device"] = toml_edit::Item::Table(table);
    write_private(path, doc.to_string().as_bytes())
}

/// Replace `path` with `bytes`: a 0600 temporary file renamed over it.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Where the config lives by default: `simply-relay-proxy/` in the user's
/// config directory (`~/Library/Application Support` on macOS,
/// `$XDG_CONFIG_HOME` or `~/.config` elsewhere, `%APPDATA%` on Windows).
pub fn default_dir() -> Option<PathBuf> {
    let env = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        env("HOME").map(|home| home.join("Library/Application Support"))
    } else if cfg!(windows) {
        env("APPDATA")
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|home| home.join(".config")))
    }?;
    Some(base.join(APP))
}

/// The link endpoint's base for a relay URL: `wss://host/devices/v1` →
/// `https://host`, `ws://` → `http://`.
pub fn link_base(relay: &str) -> Result<String, String> {
    let (scheme, rest) = if let Some(rest) = relay.strip_prefix("wss://") {
        ("https://", rest)
    } else if let Some(rest) = relay.strip_prefix("ws://") {
        ("http://", rest)
    } else {
        return Err(format!("{relay} is not a ws:// or wss:// URL"));
    };
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() {
        return Err(format!("{relay} has no host"));
    }
    Ok(format!("{scheme}{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_file_reads() {
        let config: Config = toml::from_str(
            r#"
            relay = "ws://127.0.0.1:9/devices/v1"
            [consent]
            read_only = true
            actions = { tussen = true }
            [servers.tussen]
            url = "http://127.0.0.1:7878/mcp"
            [servers.notes]
            command = ["notes", "--stdio"]
            read_only = ["search"]
            [device]
            id = "0123456789abcdef0123456789abcdef"
            key = "3f3f"
            "#,
        )
        .unwrap();
        config.check().unwrap();
        assert!(config.consent.read_only);
        assert_eq!(
            config.servers["notes"].transport(),
            Ok(Transport::Stdio(vec!["notes".into(), "--stdio".into()]))
        );
        assert!(config.servers["notes"].read_only.contains("search"));
        assert!(!format!("{:?}", config.device).contains("3f3f"), "Debug never shows the key");
    }

    #[test]
    fn defaults_and_refusals() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.relay, "wss://lumina.simply.audio/devices/v1");
        assert!(!config.consent.read_only && config.servers.is_empty());

        for bad in [
            "[servers.a]\nurl = \"http://x\"\ncommand = [\"x\"]",
            "[servers.a]",
            "[servers.a]\ncommand = []",
            "[servers.\"a.b\"]\nurl = \"http://x\"",
            "[servers.read_only]\nurl = \"http://x\"",
        ] {
            let config: Config = toml::from_str(bad).unwrap();
            assert!(config.check().is_err(), "{bad}");
        }
        assert!(toml::from_str::<Config>("[servers.a]\nurl = \"x\"\nport = 1").is_err());
    }

    #[test]
    fn the_device_is_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# my servers\n[servers.tussen]\nurl = \"http://127.0.0.1:1/mcp\"\n")
            .unwrap();
        let device =
            DeviceKey { device_id: "0123456789abcdef0123456789abcdef".into(), key: Secret::new("ab") };
        store_device(&path, &device).unwrap();
        store_device(&path, &DeviceKey { key: Secret::new("cd"), ..device }).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my servers\n"), "{text}");
        let config = Config::load(&path).unwrap();
        assert_eq!(config.device.unwrap().key, "cd");
        assert!(config.servers.contains_key("tussen"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn the_link_base_follows_the_relay() {
        assert_eq!(
            link_base("wss://lumina.simply.audio/devices/v1").unwrap(),
            "https://lumina.simply.audio"
        );
        assert_eq!(link_base("ws://127.0.0.1:9/devices/v1").unwrap(), "http://127.0.0.1:9");
        assert!(link_base("https://x").is_err());
    }
}
