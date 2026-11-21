//! The handshake frames, before MCP starts on the socket.
//!
//! The app opens `wss://<host>/devices/v1` and sends exactly one [`Hello`] as
//! its first text frame, signed with the device key the app got when it was
//! linked ([`sign_hello`]). Lumina answers with exactly one [`Welcome`] or
//! [`Refused`]. After a `Welcome`, every frame in either direction is one MCP
//! JSON-RPC message, with Lumina as the MCP client; after a `Refused`, Lumina
//! closes the socket.
//!
//! The key never travels: the hello carries the device id, a timestamp and
//! the signature over both. A `Welcome` carries a `key` only when the owner
//! re-keyed the device and this hello was signed with the previous key; the
//! app stores the new key before it serves anything.
//!
//! ```text
//! app → {"type":"hello","v":1,"app":"tussen","version":"0.9.12","label":"studio-mac",
//!        "protocol":"mcp","consent":{"read_only":true,"actions":{"perform":false}},
//!        "device_id":"0123456789abcdef0123456789abcdef","timestamp":1795000000,
//!        "signature":"sha256=…"}
//! ← Lumina {"type":"welcome","device_id":"0123…","member":"chris"}
//! ← Lumina {"type":"welcome","device_id":"0123…","member":"chris","key":"<64 hex>"}  re-keyed
//! ← Lumina {"type":"refused","reason":"unlinked","message":"This device is no longer linked."}
//! ```
//!
//! Linking and unlinking are not frames: they are HTTP calls,
//! `simply_hooks::device::{link, unlink}`.
//!
//! **Test vector**: the key
//! `3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f` (its ASCII
//! bytes are the HMAC key), timestamp `1795000000` and device id
//! `0123456789abcdef0123456789abcdef` sign as
//! `sha256=1cbf0863cfdceec87aedf35bca57030d8c31ea50c0b7ef4394491cb615153270`.
//!
//! ```
//! assert_eq!(
//!     simply_relay::wire::sign_hello(
//!         "3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f",
//!         1795000000,
//!         "0123456789abcdef0123456789abcdef",
//!     ),
//!     "sha256=1cbf0863cfdceec87aedf35bca57030d8c31ea50c0b7ef4394491cb615153270",
//! );
//! ```
//!
//! These types are plain serde and carry no rmcp type, so a listener can use
//! them as they are.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The protocol revision a [`Hello`] speaks. It is also the path segment:
/// `/devices/v1`.
pub const VERSION: u32 = 1;

/// Where a device dials by default.
pub const DEFAULT_ENDPOINT: &str = "wss://lumina.simply.audio/devices/v1";

/// The protocol served after the handshake. `acp` is reserved.
pub const PROTOCOL: &str = "mcp";

/// The `_meta` key Lumina puts on every `tools/call`, saying why it asks
/// (`"bug tussen-0042"`, `"jam #jam-chris"`). The app writes it into its
/// call log.
pub const PURPOSE_META_KEY: &str = "lumina/purpose";

/// The `_meta` key the relay stamps on every listed tool: `read_only`, or
/// the tool's action class.
pub const CLASS_META_KEY: &str = "lumina/class";

/// What a hello's signature covers: `<timestamp>.hello.<device_id>`, the
/// `simply` signing scheme's message with `hello.<device_id>` as the body.
fn hello_body(device_id: &str) -> String {
    format!("hello.{device_id}")
}

/// A hello's `signature`: `sha256=` and the hex HMAC-SHA256 over
/// `<timestamp>.hello.<device_id>`, keyed with the device key's ASCII bytes.
pub fn sign_hello(key: &str, timestamp: u64, device_id: &str) -> String {
    simply_hooks::sign::sign(key.as_bytes(), timestamp, hello_body(device_id).as_bytes())
}

/// Does `signature` sign a hello from `device_id` at `timestamp` under `key`?
/// Compared in constant time. The clock window is the listener's to check.
pub fn verify_hello(key: &str, timestamp: u64, device_id: &str, signature: &str) -> bool {
    simply_hooks::sign::verify(
        key.as_bytes(),
        timestamp,
        hello_body(device_id).as_bytes(),
        signature,
    )
}

/// The app's first and only handshake frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppFrame {
    Hello(Hello),
}

/// Lumina's one answer to a [`Hello`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LuminaFrame {
    Welcome(Welcome),
    Refused(Refused),
}

/// "This is me, and this is what I let you ask."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// [`VERSION`].
    pub v: u32,
    /// The app's name: `tussen`, `flux`.
    pub app: String,
    /// The app's version.
    pub version: String,
    /// The member's name for this copy of the app: `studio-mac`.
    pub label: String,
    /// What the app serves after the handshake: [`PROTOCOL`].
    #[serde(default = "mcp")]
    pub protocol: String,
    /// The switches as they are now. Informational for Lumina (it shows them
    /// in `/devices`); the app enforces them whatever Lumina thinks.
    pub consent: ConsentReport,
    /// 32 lowercase hex digits, from the link.
    pub device_id: String,
    /// Unix seconds when the hello was signed.
    pub timestamp: u64,
    /// [`sign_hello`] over `timestamp` and `device_id`.
    pub signature: String,
}

fn mcp() -> String {
    PROTOCOL.to_owned()
}

/// The switches, as reported in a [`Hello`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentReport {
    /// "Let Lumina run diagnostics."
    pub read_only: bool,
    /// One switch per action class, by class name (`perform`).
    #[serde(default)]
    pub actions: BTreeMap<String, bool>,
}

/// "You're in." MCP starts with the next frame.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub device_id: String,
    /// Whose device this is, as Lumina names the member, so the app can show
    /// "linked to @name".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The device's new key, only when the owner re-keyed it and the hello
    /// was signed with the previous one. Store it before serving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// "No." Lumina closes the socket after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refused {
    pub reason: RefusedReason,
    /// One sentence for the member.
    #[serde(default)]
    pub message: String,
}

/// Why a hello was refused, which decides what the app does next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusedReason {
    /// No such device, or a bad signature: the app's link is gone. It stops
    /// dialling and offers to link again.
    Unlinked,
    /// The timestamp is outside Lumina's window. The app retries slowly and
    /// tells the member to check the clock.
    Clock,
    /// This link may not dial the relay (a member's own link, not a device
    /// the owner made). The app keeps the link and retries at its slowest.
    Scope,
    /// Devices are switched off on Lumina's side (the owner's kill switch).
    /// The app keeps its link and retries at its slowest.
    Disabled,
    /// Too many attempts, or too many sockets. Retry with backoff.
    Limit,
    /// This protocol revision, or the hello's `protocol`, is not served. The
    /// app stops dialling until it is updated.
    Version,
    /// A reason this app does not know yet: treated as [`Limit`](Self::Limit).
    #[serde(other)]
    Other,
}

// A key is a secret: never in a Debug line.
impl std::fmt::Debug for Welcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Welcome")
            .field("device_id", &self.device_id)
            .field("member", &self.member)
            .field("key", &self.key.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &str = "3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f3f";
    const DEVICE: &str = "0123456789abcdef0123456789abcdef";

    /// The vector every side of the wire pins.
    #[test]
    fn the_hello_vector_signs_and_verifies() {
        let signature = sign_hello(KEY, 1795000000, DEVICE);
        assert_eq!(
            signature,
            "sha256=1cbf0863cfdceec87aedf35bca57030d8c31ea50c0b7ef4394491cb615153270"
        );
        assert!(verify_hello(KEY, 1795000000, DEVICE, &signature));
        assert!(!verify_hello(KEY, 1795000001, DEVICE, &signature), "the timestamp is signed");
        assert!(
            !verify_hello(KEY, 1795000000, "0123456789abcdef0123456789abcdee", &signature),
            "the device id is signed"
        );
    }

    #[test]
    fn the_hello_serialises_as_documented() {
        let hello = AppFrame::Hello(Hello {
            v: VERSION,
            app: "tussen".into(),
            version: "0.9.12".into(),
            label: "studio-mac".into(),
            protocol: PROTOCOL.into(),
            consent: ConsentReport {
                read_only: true,
                actions: BTreeMap::from([("perform".into(), false)]),
            },
            device_id: DEVICE.into(),
            timestamp: 1795000000,
            signature: sign_hello(KEY, 1795000000, DEVICE),
        });
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            concat!(
                r#"{"type":"hello","v":1,"app":"tussen","version":"0.9.12","label":"studio-mac","#,
                r#""protocol":"mcp","consent":{"read_only":true,"actions":{"perform":false}},"#,
                r#""device_id":"0123456789abcdef0123456789abcdef","timestamp":1795000000,"#,
                r#""signature":"sha256=1cbf0863cfdceec87aedf35bca57030d8c31ea50c0b7ef4394491cb615153270"}"#,
            )
        );
        let back: AppFrame = serde_json::from_str(&serde_json::to_string(&hello).unwrap()).unwrap();
        assert_eq!(back, hello);
    }

    #[test]
    fn a_hello_without_a_protocol_reads_as_mcp() {
        let AppFrame::Hello(hello) = serde_json::from_value(json!({
            "type": "hello", "v": 1, "app": "tussen", "version": "0.9.12", "label": "",
            "consent": {"read_only": false}, "device_id": DEVICE, "timestamp": 1,
            "signature": "sha256=00"
        }))
        .unwrap();
        assert_eq!(hello.protocol, "mcp");
    }

    #[test]
    fn welcomes_read_with_and_without_a_new_key() {
        let plain: LuminaFrame = serde_json::from_value(json!({
            "type": "welcome", "device_id": DEVICE, "member": "chris"
        }))
        .unwrap();
        let LuminaFrame::Welcome(plain) = plain else { panic!("welcome") };
        assert_eq!((plain.member.as_deref(), plain.key.as_deref()), (Some("chris"), None));

        let rekeyed: LuminaFrame = serde_json::from_value(json!({
            "type": "welcome", "device_id": DEVICE, "member": "chris", "key": KEY
        }))
        .unwrap();
        let LuminaFrame::Welcome(rekeyed) = rekeyed else { panic!("welcome") };
        assert_eq!(rekeyed.key.as_deref(), Some(KEY));
        assert!(!format!("{rekeyed:?}").contains(KEY), "Debug never shows a key");
    }

    #[test]
    fn every_refused_reason_reads() {
        let read = |reason: &str| -> RefusedReason {
            let frame: LuminaFrame = serde_json::from_value(json!({
                "type": "refused", "reason": reason, "message": "no"
            }))
            .unwrap();
            let LuminaFrame::Refused(refused) = frame else { panic!("refused") };
            assert_eq!(refused.message, "no");
            refused.reason
        };
        assert_eq!(read("unlinked"), RefusedReason::Unlinked);
        assert_eq!(read("clock"), RefusedReason::Clock);
        assert_eq!(read("scope"), RefusedReason::Scope);
        assert_eq!(read("disabled"), RefusedReason::Disabled);
        assert_eq!(read("limit"), RefusedReason::Limit);
        assert_eq!(read("version"), RefusedReason::Version);
        assert_eq!(read("something_new"), RefusedReason::Other);
    }
}
