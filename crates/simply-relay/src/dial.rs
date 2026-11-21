//! The connection loop: take the app's key, dial, say a signed hello, store
//! a re-keyed device's new key, serve, and when the socket goes, back off and
//! dial again. What a refusal leads to is decided here, per reason.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rmcp::ServerHandler;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream};

use crate::frames::{json_frames, FrameSink, FrameStream};
use crate::gate::Gated;
use crate::wire::{
    sign_hello, AppFrame, Hello, LuminaFrame, Refused, RefusedReason, Welcome, PROTOCOL, VERSION,
};
use crate::{Command, DeviceKey, Secret, Shared, Status};

/// The largest frame either side accepts.
pub const MAX_FRAME: usize = 1 << 20;

type Socket = MaybeTlsStream<TcpStream>;

enum Failure {
    Refused(Refused),
    Failed(String),
}

fn tls() -> Connector {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    let config = CONFIG.get_or_init(|| {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Arc::new(config)
    });
    Connector::Rustls(config.clone())
}

/// Open the socket, send `first`, read Lumina's answer.
async fn handshake(
    shared: &Shared,
    endpoint: &str,
    first: AppFrame,
) -> Result<(FrameSink<Socket, AppFrame>, FrameStream<Socket, LuminaFrame>, Welcome), Failure> {
    use futures::{SinkExt, StreamExt};
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let attempt = async {
        let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
            endpoint,
            Some(config),
            true,
            Some(tls()),
        )
        .await
        .map_err(|e| Failure::Failed(e.to_string()))?;
        let (mut sink, mut stream) =
            json_frames::<_, AppFrame, LuminaFrame>(ws, Some(shared.liveness));
        sink.send(first).await.map_err(|e| Failure::Failed(e.to_string()))?;
        match stream.next().await {
            Some(LuminaFrame::Welcome(welcome)) => Ok((sink, stream, welcome)),
            Some(LuminaFrame::Refused(refused)) => Err(Failure::Refused(refused)),
            None => Err(Failure::Failed("Lumina closed the connection without answering".into())),
        }
    };
    match tokio::time::timeout(shared.handshake_timeout, attempt).await {
        Ok(result) => result,
        Err(_) => Err(Failure::Failed("Lumina did not answer in time".into())),
    }
}

/// A hello signed now with `device`'s key.
fn hello(shared: &Shared, device: &DeviceKey) -> AppFrame {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    AppFrame::Hello(Hello {
        v: VERSION,
        app: shared.app.clone(),
        version: shared.version.clone(),
        label: shared.label.clone(),
        protocol: PROTOCOL.to_owned(),
        consent: shared.consent().report(),
        device_id: device.device_id.clone(),
        timestamp,
        signature: sign_hello(device.key.expose(), timestamp, &device.device_id),
    })
}

/// The app's key, from its callback, off the async threads (it may read the
/// keychain).
async fn device_key(shared: &Shared) -> Option<DeviceKey> {
    let source = shared.key.clone();
    tokio::task::spawn_blocking(move || source()).await.ok().flatten()
}

/// If the owner re-keyed the device, hand the welcome's new key to the app,
/// off the async threads.
async fn renew(shared: &Shared, device: &DeviceKey, welcome: &Welcome) -> Result<(), String> {
    let Some(key) = &welcome.key else { return Ok(()) };
    let device = DeviceKey { device_id: device.device_id.clone(), key: Secret::new(key) };
    let Some(sink) = shared.new_key.clone() else {
        return Err("the app takes no new key".into());
    };
    tokio::task::spawn_blocking(move || sink(&device)).await.unwrap_or_else(|e| Err(e.to_string()))
}

/// `delay` ±25 %, from the clock's low bits: enough to spread a fleet of
/// devices reconnecting after Lumina restarts.
fn jittered(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let factor = 0.75 + f64::from(nanos % 1000) / 2000.0;
    delay.mul_f64(factor)
}

pub(crate) async fn run<S, F>(
    shared: Arc<Shared>,
    factory: F,
    mut commands: mpsc::UnboundedReceiver<Command>,
) where
    S: ServerHandler,
    F: Fn() -> S + Send + Sync + 'static,
{
    let mut delay = shared.backoff.min;
    let mut pending: Option<Command> = None;
    // Set by a refusal that retrying cannot fix, or a disconnect; cleared by
    // `Relay::connect`.
    let mut parked = false;

    loop {
        match pending.take() {
            Some(Command::Shutdown) => return,
            Some(Command::Disconnect(done)) => {
                parked = true;
                shared.set_status(Status::Disconnected);
                let _ = done.send(());
                continue;
            }
            Some(Command::Connect) => {
                parked = false;
                delay = shared.backoff.min;
            }
            None => {}
        }
        if parked {
            pending = Some(commands.recv().await.unwrap_or(Command::Shutdown));
            continue;
        }

        let Some(device) = device_key(&shared).await else {
            shared.set_status(Status::NotLinked);
            pending = Some(commands.recv().await.unwrap_or(Command::Shutdown));
            continue;
        };
        shared.set_status(Status::Connecting);
        let first = hello(&shared, &device);

        let attempt = tokio::select! {
            attempt = handshake(&shared, &shared.endpoint, first) => attempt,
            command = commands.recv() => {
                pending = Some(command.unwrap_or(Command::Shutdown));
                continue;
            }
        };

        // Unserved if a new key can't be stored: the next hello, still signed
        // with the old key, is handed the new key again.
        let attempt = match attempt {
            Ok((sink, stream, welcome)) => match renew(&shared, &device, &welcome).await {
                Ok(()) => Ok((sink, stream, welcome)),
                Err(e) => {
                    Err(Failure::Failed(format!("could not store the device's new key: {e}")))
                }
            },
            failed => failed,
        };
        let (failure, reason) = match attempt {
            Ok((sink, stream, welcome)) => {
                let started = Instant::now();
                let served =
                    serve(&shared, &factory, sink, stream, &welcome, &mut commands, &mut pending)
                        .await;
                if pending.is_some() {
                    continue;
                }
                if started.elapsed() >= shared.backoff.max {
                    delay = shared.backoff.min;
                }
                (served.err().unwrap_or_else(|| "the connection closed".into()), None)
            }
            Err(Failure::Refused(refused)) => {
                tracing::warn!("relay: Lumina refused: {:?}: {}", refused.reason, refused.message);
                match refused.reason {
                    RefusedReason::Unlinked => {
                        parked = true;
                        shared.set_status(Status::Unlinked { message: refused.message });
                        continue;
                    }
                    RefusedReason::Version => {
                        parked = true;
                        shared.set_status(Status::Stopped { message: refused.message });
                        continue;
                    }
                    // Nothing the next few attempts can change: the clock
                    // needs the member, scope and the kill switch the owner.
                    RefusedReason::Clock | RefusedReason::Scope | RefusedReason::Disabled => {
                        delay = shared.backoff.max;
                    }
                    RefusedReason::Limit | RefusedReason::Other => {}
                }
                (format!("Lumina refused: {}", refused.message), Some(refused.reason))
            }
            Err(Failure::Failed(error)) => {
                tracing::warn!("relay: could not reach Lumina: {error}");
                (error, None)
            }
        };

        let wait = jittered(delay);
        shared.set_status(Status::Retrying { after: wait, error: failure, reason });
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            command = commands.recv() => pending = Some(command.unwrap_or(Command::Shutdown)),
        }
        delay = (delay * 2).min(shared.backoff.max);
    }
}

/// Serve MCP until the socket goes or a command arrives (left in
/// `pending`).
async fn serve<S, F>(
    shared: &Arc<Shared>,
    factory: &F,
    sink: FrameSink<Socket, AppFrame>,
    stream: FrameStream<Socket, LuminaFrame>,
    welcome: &Welcome,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    pending: &mut Option<Command>,
) -> Result<(), String>
where
    S: ServerHandler,
    F: Fn() -> S,
{
    let gated = Gated::new(factory(), shared.clone());
    let transport = (
        sink.retype::<rmcp::model::ServerJsonRpcMessage>(),
        stream.retype::<rmcp::model::ClientJsonRpcMessage>(),
    );
    let serving = tokio::select! {
        serving = rmcp::serve_server(gated, transport) => serving,
        command = commands.recv() => {
            *pending = Some(command.unwrap_or(Command::Shutdown));
            return Ok(());
        }
    };
    let running = serving.map_err(|e| format!("MCP did not start: {e}"))?;
    shared.set_status(Status::Connected {
        device_id: welcome.device_id.clone(),
        member: welcome.member.clone(),
    });
    let token = running.cancellation_token();
    let waiting = running.waiting();
    tokio::pin!(waiting);
    let result = loop {
        tokio::select! {
            _ = &mut waiting => break Ok(()),
            command = commands.recv() => match command {
                // Already connected: nothing to do.
                Some(Command::Connect) => {}
                command => {
                    *pending = Some(command.unwrap_or(Command::Shutdown));
                    token.cancel();
                    break Ok(());
                }
            },
        }
    };
    shared.set_peer(None);
    result
}
