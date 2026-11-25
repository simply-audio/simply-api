//! The config file: where Lumina is, the linked devices, the switches and the
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
//! [[devices]]                      # written by `simply-relay-proxy pair`
//! id = "0123456789abcdef0123456789abcdef"
//! key = "…"
//! server = "Studio Discord"
//! member = "chris"
//!
//! [devices.consent]                # optional per-device override of [consent]
//! actions = { tussen = false }
//! ```
//!
//! The legacy single `[device]` table is still accepted alongside `[[devices]]`.
//! The file holds device keys, so the proxy writes it readable by its owner
//! only.

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
    /// The shared default switches; all off by default.
    #[serde(default)]
    pub consent: Consent,
    /// The local MCP servers, by name.
    #[serde(default)]
    pub servers: BTreeMap<String, Server>,
    /// Legacy single-device `[device]` table; kept for backwards compatibility.
    #[serde(default)]
    pub device: Option<Device>,
    /// Linked devices (`[[devices]]`), written by `pair`.
    #[serde(default)]
    pub devices: Vec<Device>,
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

/// Per-device override overlaid onto the top-level `[consent]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentOverride {
    /// Override top-level `consent.read_only` when set.
    #[serde(default)]
    pub read_only: Option<bool>,
    /// Per-server action switches overlaid onto top-level `consent.actions`.
    #[serde(default)]
    pub actions: BTreeMap<String, bool>,
}

impl ConsentOverride {
    /// Overlay this override onto `base`, returning the effective switches.
    pub fn apply(&self, base: &Consent) -> Consent {
        let mut merged = base.clone();
        if let Some(read_only) = self.read_only {
            merged.read_only = read_only;
        }
        for (class, on) in &self.actions {
            merged.actions.insert(class.clone(), *on);
        }
        merged
    }

    /// True when no field overrides the top-level `[consent]`.
    pub fn is_empty(&self) -> bool {
        self.read_only.is_none() && self.actions.is_empty()
    }
}

/// One linked device (`[[devices]]`, or the legacy `[device]` table).
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub id: String,
    pub key: String,
    /// The Discord server name recorded when pairing.
    #[serde(default)]
    pub server: Option<String>,
    /// The member name recorded when pairing.
    #[serde(default)]
    pub member: Option<String>,
    /// Optional per-device consent override.
    #[serde(default)]
    pub consent: Option<ConsentOverride>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("id", &self.id)
            .field("server", &self.server)
            .field("member", &self.member)
            .field("consent", &self.consent)
            .finish_non_exhaustive()
    }
}

impl Device {
    pub fn key(&self) -> DeviceKey {
        DeviceKey { device_id: self.id.clone(), key: Secret::new(&self.key) }
    }

    /// Effective consent switches for this device: top-level `base` with any
    /// per-device override overlaid.
    pub fn effective_consent(&self, base: &Consent) -> Consent {
        match &self.consent {
            Some(override_consent) => override_consent.apply(base),
            None => base.clone(),
        }
    }

    /// First 8 characters of the device id, for compact display.
    pub fn short_id(&self) -> &str {
        match self.id.char_indices().nth(8) {
            Some((idx, _)) => &self.id[..idx],
            None => &self.id,
        }
    }

    /// Server name if known, otherwise the short device id.
    pub fn display_name(&self) -> &str {
        self.server.as_deref().filter(|s| !s.is_empty()).unwrap_or_else(|| self.short_id())
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

    /// All linked devices: the legacy `[device]` (if present) followed by
    /// every `[[devices]]` entry.
    pub fn all_devices(&self) -> Vec<&Device> {
        self.device.iter().chain(self.devices.iter()).collect()
    }

    /// Look up a linked device by exact `id`.
    pub fn find_device(&self, id: &str) -> Option<&Device> {
        self.all_devices().into_iter().find(|d| d.id == id)
    }

    /// Server names become the first part of every tool name and the name
    /// of their action switch. Device ids must be non-empty, safe as a
    /// directory name, and unique across `[device]` and `[[devices]]`.
    fn check(&self) -> Result<(), String> {
        for (name, server) in &self.servers {
            if name.is_empty() || name.contains('.') || name == "read_only" {
                return Err(format!(
                    "server name {name:?}: it may not be empty, contain a dot or be `read_only`"
                ));
            }
            server.transport().map_err(|e| format!("[servers.{name}] {e}"))?;
        }
        let mut seen = BTreeSet::new();
        for device in self.all_devices() {
            if device.id.is_empty()
                || device.id == "."
                || device.id == ".."
                || device.id.contains('/')
                || device.id.contains('\\')
            {
                return Err(format!("invalid device id {:?}", device.id));
            }
            if !seen.insert(&device.id) {
                return Err(format!("duplicate device id {:?}", device.id));
            }
        }
        Ok(())
    }
}

/// Why resolving or removing a device failed.
#[derive(Debug)]
pub enum RemoveError {
    Io(io::Error),
    /// No device matched `target` by id or server name.
    NotFound(String),
    /// More than one device matched `target` by server name.
    Ambiguous {
        target: String,
        matches: Vec<String>,
    },
}

impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoveError::Io(e) => write!(f, "{e}"),
            RemoveError::NotFound(target) => write!(f, "no linked device matches {target:?}"),
            RemoveError::Ambiguous { target, matches } => write!(
                f,
                "more than one device matches server {target:?} ({}); pass the device id instead",
                matches.join(", ")
            ),
        }
    }
}

impl std::error::Error for RemoveError {}

impl From<io::Error> for RemoveError {
    fn from(e: io::Error) -> Self {
        RemoveError::Io(e)
    }
}

/// Resolve `target` against `config`: exact `id` match first, then
/// case-insensitive `server` match.
pub fn resolve_device<'a>(config: &'a Config, target: &str) -> Result<&'a Device, RemoveError> {
    if let Some(device) = config.find_device(target) {
        return Ok(device);
    }
    let matches: Vec<&Device> = config
        .all_devices()
        .into_iter()
        .filter(|d| d.server.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(target)))
        .collect();
    match matches.as_slice() {
        [] => Err(RemoveError::NotFound(target.to_owned())),
        [only] => Ok(*only),
        many => Err(RemoveError::Ambiguous {
            target: target.to_owned(),
            matches: many.iter().map(|d| d.id.clone()).collect(),
        }),
    }
}

/// Update `device`'s key in place if its `device_id` already exists (in
/// `[device]` or `[[devices]]`), or append a new `[[devices]]` entry,
/// keeping every other device and the rest of the file intact; the file ends
/// up readable by its owner only.
pub fn store_device(path: &Path, device: &DeviceKey) -> io::Result<()> {
    store_linked_device(path, device, None, None)
}

/// Like [`store_device`], also recording optional `server` and `member`
/// display metadata (when non-empty).
pub fn store_linked_device(
    path: &Path,
    device: &DeviceKey,
    server: Option<&str>,
    member: Option<&str>,
) -> io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| invalid(path, e))?;
    let server = server.filter(|s| !s.is_empty());
    let member = member.filter(|m| !m.is_empty());

    let apply = |table: &mut toml_edit::Table| {
        table["id"] = toml_edit::value(device.device_id.clone());
        table["key"] = toml_edit::value(device.key.expose());
        if let Some(s) = server {
            table["server"] = toml_edit::value(s);
        }
        if let Some(m) = member {
            table["member"] = toml_edit::value(m);
        }
    };

    // 1. Update legacy `[device]` in place if it has the same id.
    let updated_legacy = doc
        .get_mut("device")
        .and_then(|item| item.as_table_mut())
        .filter(|table| table.get("id").and_then(|v| v.as_str()) == Some(device.device_id.as_str()))
        .map(|table| {
            apply(table);
        })
        .is_some();

    // 2. Otherwise update matching `[[devices]]` entry in place, or append one.
    if !updated_legacy {
        let mut updated_entry = false;
        if let Some(aot) = doc.get_mut("devices").and_then(|item| item.as_array_of_tables_mut()) {
            for table in aot.iter_mut() {
                if table.get("id").and_then(|v| v.as_str()) == Some(device.device_id.as_str()) {
                    apply(table);
                    updated_entry = true;
                    break;
                }
            }
            if !updated_entry {
                let mut table = toml_edit::Table::new();
                apply(&mut table);
                aot.push(table);
                updated_entry = true;
            }
        }
        if !updated_entry {
            let mut aot = toml_edit::ArrayOfTables::new();
            let mut table = toml_edit::Table::new();
            apply(&mut table);
            aot.push(table);
            doc["devices"] = toml_edit::Item::ArrayOfTables(aot);
        }
    }

    write_private(path, doc.to_string().as_bytes())
}

/// Remove the device matching `target` (exact id or unique server name) from
/// the file at `path`, keeping all other devices and the rest of the file
/// intact. Returns the removed device.
pub fn remove_device(path: &Path, target: &str) -> Result<Device, RemoveError> {
    let config = Config::load(path)?;
    let removed = resolve_device(&config, target)?.clone();

    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| invalid(path, e))?;

    let is_legacy = doc
        .get("device")
        .and_then(|item| item.as_table())
        .and_then(|table| table.get("id"))
        .and_then(|v| v.as_str())
        == Some(removed.id.as_str());

    if is_legacy {
        doc.remove("device");
    } else if let Some(aot) = doc.get_mut("devices").and_then(|item| item.as_array_of_tables_mut())
    {
        let pos = aot.iter().position(|table| {
            table.get("id").and_then(|v| v.as_str()) == Some(removed.id.as_str())
        });
        if let Some(pos) = pos {
            aot.remove(pos);
        }
        if aot.is_empty() {
            doc.remove("devices");
        }
    }

    write_private(path, doc.to_string().as_bytes())?;
    Ok(removed)
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
        let device = DeviceKey {
            device_id: "0123456789abcdef0123456789abcdef".into(),
            key: Secret::new("ab"),
        };
        store_device(&path, &device).unwrap();
        store_device(&path, &DeviceKey { key: Secret::new("cd"), ..device }).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my servers\n"), "{text}");
        let config = Config::load(&path).unwrap();
        assert_eq!(config.devices.len(), 1);
        assert_eq!(config.devices[0].key, "cd");
        assert!(config.servers.contains_key("tussen"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn multi_device_loading_and_legacy_compat() {
        let config: Config = toml::from_str(
            r#"
            [consent]
            read_only = true
            actions = { tussen = true, flux = false }

            [device]
            id = "aaaa"
            key = "k1"

            [[devices]]
            id = "bbbb"
            key = "k2"
            server = "Band Server"
            member = "chris"

            [devices.consent]
            actions = { tussen = false, flux = true }

            [[devices]]
            id = "cccc"
            key = "k3"
            server = "Studio"

            [devices.consent]
            read_only = false
            "#,
        )
        .unwrap();
        config.check().unwrap();
        let all = config.all_devices();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "aaaa");
        assert_eq!(all[1].id, "bbbb");
        assert_eq!(all[2].id, "cccc");

        // Legacy [device] inherits top-level [consent].
        let c0 = all[0].effective_consent(&config.consent);
        assert!(c0.read_only);
        assert_eq!(c0.actions.get("tussen"), Some(&true));
        assert_eq!(c0.actions.get("flux"), Some(&false));

        // Second device overrides actions only; read_only stays true.
        let c1 = all[1].effective_consent(&config.consent);
        assert!(c1.read_only);
        assert_eq!(c1.actions.get("tussen"), Some(&false));
        assert_eq!(c1.actions.get("flux"), Some(&true));

        // Third device overrides read_only only; actions stay inherited.
        let c2 = all[2].effective_consent(&config.consent);
        assert!(!c2.read_only);
        assert_eq!(c2.actions.get("tussen"), Some(&true));
        assert_eq!(c2.actions.get("flux"), Some(&false));

        // Debug never leaks keys.
        let dbg = format!("{:?}", all);
        assert!(!dbg.contains("k1") && !dbg.contains("k2") && !dbg.contains("k3"));
    }

    #[test]
    fn duplicate_and_invalid_device_ids_are_rejected() {
        for bad in [
            // Duplicate between [device] and [[devices]].
            "[device]\nid = \"dup\"\nkey = \"1\"\n[[devices]]\nid = \"dup\"\nkey = \"2\"\n",
            // Duplicate within [[devices]].
            "[[devices]]\nid = \"dup\"\nkey = \"1\"\n[[devices]]\nid = \"dup\"\nkey = \"2\"\n",
            // Empty or path-unsafe ids.
            "[[devices]]\nid = \"\"\nkey = \"1\"\n",
            "[[devices]]\nid = \"../escape\"\nkey = \"1\"\n",
            "[[devices]]\nid = \"a/b\"\nkey = \"1\"\n",
        ] {
            let config: Config = toml::from_str(bad).unwrap();
            assert!(config.check().is_err(), "{bad}");
        }
    }

    #[test]
    fn store_preserves_legacy_device_and_consent_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep me\n\
             [consent]\nread_only = true\n\n\
             [device]\nid = \"legacy1\"\nkey = \"old_legacy\"\n\n\
             [[devices]]\nid = \"dev2\"\nkey = \"old_dev2\"\nserver = \"Alpha\"\nmember = \"alice\"\n\
             [devices.consent]\nread_only = false\nactions = { tussen = true }\n",
        )
        .unwrap();

        // Pairing a third device appends [[devices]] without losing [device] or dev2.
        let dev3 = DeviceKey { device_id: "dev3".into(), key: Secret::new("key3") };
        store_linked_device(&path, &dev3, Some("Beta"), Some("bob")).unwrap();

        // Re-keying legacy [device] updates it in place without touching [[devices]].
        let legacy_new = DeviceKey { device_id: "legacy1".into(), key: Secret::new("new_legacy") };
        store_device(&path, &legacy_new).unwrap();

        // Re-keying dev2 updates only its key, preserving server, member, and [devices.consent].
        let dev2_new = DeviceKey { device_id: "dev2".into(), key: Secret::new("new_dev2") };
        store_device(&path, &dev2_new).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# keep me\n"), "{text}");
        let config = Config::load(&path).unwrap();
        assert_eq!(config.device.as_ref().unwrap().id, "legacy1");
        assert_eq!(config.device.as_ref().unwrap().key, "new_legacy");
        assert_eq!(config.devices.len(), 2);
        assert_eq!(config.devices[0].id, "dev2");
        assert_eq!(config.devices[0].key, "new_dev2");
        assert_eq!(config.devices[0].server.as_deref(), Some("Alpha"));
        assert_eq!(config.devices[0].member.as_deref(), Some("alice"));
        let consent = config.devices[0].consent.as_ref().unwrap();
        assert_eq!(consent.read_only, Some(false));
        assert_eq!(consent.actions.get("tussen"), Some(&true));
        assert_eq!(config.devices[1].id, "dev3");
        assert_eq!(config.devices[1].key, "key3");
        assert_eq!(config.devices[1].server.as_deref(), Some("Beta"));
        assert_eq!(config.devices[1].member.as_deref(), Some("bob"));
    }

    #[test]
    fn remove_device_by_id_or_server_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep comment\n\
             [servers.tussen]\nurl = \"http://127.0.0.1:9097/mcp\"\n\n\
             [device]\nid = \"legacy1\"\nkey = \"k0\"\nserver = \"Old\"\n\n\
             [[devices]]\nid = \"d1\"\nkey = \"k1\"\nserver = \"Shared\"\n\
             [devices.consent]\nread_only = false\n\n\
             [[devices]]\nid = \"d2\"\nkey = \"k2\"\nserver = \"Shared\"\n\n\
             [[devices]]\nid = \"d3\"\nkey = \"k3\"\nserver = \"Unique\"\n",
        )
        .unwrap();

        // Unknown target -> NotFound.
        assert!(matches!(remove_device(&path, "nope"), Err(RemoveError::NotFound(_))));

        // Ambiguous server name -> Ambiguous with both ids.
        match remove_device(&path, "Shared") {
            Err(RemoveError::Ambiguous { matches, .. }) => assert_eq!(matches, ["d1", "d2"]),
            other => panic!("expected Ambiguous, got {other:?}"),
        }

        // Remove unique server name (case-insensitive).
        let removed = remove_device(&path, "unique").unwrap();
        assert_eq!(removed.id, "d3");

        // Remove one of the ambiguous ones by exact id.
        let removed = remove_device(&path, "d1").unwrap();
        assert_eq!(removed.id, "d1");

        // Now "Shared" is unique and works by server name.
        let removed = remove_device(&path, "Shared").unwrap();
        assert_eq!(removed.id, "d2");

        // Legacy [device] is still there; remove it by id.
        let config = Config::load(&path).unwrap();
        assert_eq!(config.all_devices().len(), 1);
        assert_eq!(config.device.as_ref().unwrap().id, "legacy1");
        assert!(config.devices.is_empty());

        let removed = remove_device(&path, "legacy1").unwrap();
        assert_eq!(removed.id, "legacy1");

        let config = Config::load(&path).unwrap();
        assert!(config.all_devices().is_empty());
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# keep comment\n"));
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
