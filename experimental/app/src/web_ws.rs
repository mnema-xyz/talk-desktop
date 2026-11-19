//! The WebSocket of the selected instance, opened by the host for the app.
//! The page cannot do it itself: its origin is the host's, so the instance
//! would see the wrong `Origin` and no session cookie. Only `/api/ws` can be
//! opened, text frames are relayed both ways, and everything stays bounded:
//! sockets, queued messages and the time a write may take.
use crate::web_proxy::{Instance, now};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::{
    Message, client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
};

pub const MAX_FRAME: usize = 1 << 20;
const MAX_SOCKETS: usize = 4;
const MAX_OUTGOING: usize = 1 << 20;
/// Messages waiting to be written to one socket; a page that outruns its
/// connection gets an error from `send` instead of growing the queue.
const MAX_QUEUED: usize = 32;
/// How long writing a frame or the close handshake may take before the socket
/// is dropped, so a stalled peer cannot hold one of the few slots.
const WRITE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(5)
};

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WsEvent {
    Open {
        id: u64,
    },
    Message {
        id: u64,
        data: String,
    },
    /// `code` is the close code the instance sent, so the page can tell a
    /// replaced session (4001) from a dropped connection; absent otherwise.
    Close {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<u16>,
    },
}

/// What the page can do to an open socket: queue text, ask it to close.
#[derive(Clone)]
struct Handle {
    queue: mpsc::Sender<String>,
    /// Closing does not go through the queue, which may be full.
    close: Arc<Notify>,
}

#[derive(Default)]
pub struct Sockets {
    next: AtomicU64,
    open: Mutex<HashMap<u64, Handle>>,
}

impl Sockets {
    pub fn send(&self, id: u64, text: String) -> Result<(), String> {
        if text.len() > MAX_OUTGOING {
            return Err("message too large".into());
        }
        self.handle(id)?.queue.try_send(text).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => "socket busy".to_owned(),
            mpsc::error::TrySendError::Closed(_) => "socket closed".to_owned(),
        })
    }

    pub fn close(&self, id: u64) {
        if let Ok(handle) = self.handle(id) {
            handle.close.notify_one();
        }
    }

    pub fn close_all(&self) {
        if let Ok(open) = self.open.lock() {
            for handle in open.values() {
                handle.close.notify_one();
            }
        }
    }

    /// The page that opened the sockets is gone (reload, navigation): they
    /// close, and their slots are free at once for the page that follows,
    /// however long a stalled peer takes to let go.
    pub fn document_replaced(&self) {
        if let Ok(mut open) = self.open.lock() {
            for (_, handle) in open.drain() {
                handle.close.notify_one();
            }
        }
    }

    fn handle(&self, id: u64) -> Result<Handle, String> {
        self.open
            .lock()
            .map_err(|_| "busy")?
            .get(&id)
            .cloned()
            .ok_or_else(|| "unknown socket".to_owned())
    }

    #[cfg(test)]
    fn count(&self) -> usize {
        self.open.lock().unwrap().len()
    }
}

/// What a page may ask the host to open: the instance's `/api/ws`, with the
/// one question the server knows, `dm=1` (also send conversation events, 1.1).
/// Anything else, in the path or the query, is refused instead of passed on.
pub fn socket_request(path: &str) -> Result<bool, String> {
    match path {
        "/api/ws" => Ok(false),
        "/api/ws?dm=1" => Ok(true),
        _ => Err("Only /api/ws can be opened.".into()),
    }
}

fn socket_url(instance: &Instance, dm: bool) -> Option<url::Url> {
    let mut url = instance.origin.join("api/ws").ok()?;
    if dm {
        url.set_query(Some("dm=1"));
    }
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme).ok()?;
    Some(url)
}

async fn connect(
    instance: &Instance,
    dm: bool,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    (),
> {
    let url = socket_url(instance, dm).ok_or(())?;
    let mut request = url.as_str().into_client_request().map_err(|_| ())?;
    let headers = request.headers_mut();
    headers.insert(
        "Origin",
        HeaderValue::from_str(&instance.origin.origin().ascii_serialization()).map_err(|_| ())?,
    );
    if let Some(cookie) = instance.jar.header(now()) {
        let mut value = HeaderValue::from_str(&cookie).map_err(|_| ())?;
        value.set_sensitive(true);
        headers.insert("Cookie", value);
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(
        crate::web_tls::client_config().map_err(|why| crate::web_tls::log_failure(&why))?,
    ));
    let connecting = tokio_tungstenite::connect_async_tls_with_config(
        request,
        Some(config),
        false,
        Some(connector),
    );
    match tokio::time::timeout(Duration::from_secs(10), connecting).await {
        Ok(Ok((stream, _))) => Ok(stream),
        _ => Err(()),
    }
}

/// Starts a connection and returns its id at once; `emit` receives `Open`
/// when the instance accepted it, the messages, and `Close` exactly once.
pub fn open(
    sockets: &Arc<Sockets>,
    instance: Arc<Instance>,
    dm: bool,
    emit: impl Fn(WsEvent) -> bool + Send + Sync + 'static,
) -> Result<u64, String> {
    let id = {
        let mut open = sockets.open.lock().map_err(|_| "busy")?;
        if open.len() >= MAX_SOCKETS {
            return Err("too many sockets".into());
        }
        let id = sockets.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (queue, rx) = mpsc::channel(MAX_QUEUED);
        let close = Arc::new(Notify::new());
        open.insert(
            id,
            Handle {
                queue,
                close: Arc::clone(&close),
            },
        );
        let sockets = Arc::clone(sockets);
        tauri::async_runtime::spawn(async move {
            let code = run(id, &instance, dm, rx, &close, &emit).await;
            if let Ok(mut open) = sockets.open.lock() {
                open.remove(&id);
            }
            emit(WsEvent::Close { id, code });
        });
        id
    };
    Ok(id)
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Writes one text frame; false when it could not be written in time.
async fn write_text(ws: &mut Socket, text: String) -> bool {
    let sent = tokio::time::timeout(WRITE_TIMEOUT, ws.send(Message::text(text))).await;
    matches!(sent, Ok(Ok(())))
}

/// Hands a frame from the server to the page. `Some(goodbye)` ends the
/// connection (`true` when this side should say goodbye, plus the close code
/// the instance sent, if any), `None` carries on.
fn on_incoming(
    id: u64,
    incoming: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    emit: &impl Fn(WsEvent) -> bool,
) -> Option<(bool, Option<u16>)> {
    match incoming {
        Some(Ok(Message::Text(text))) => {
            let data = text.as_str().to_owned();
            (!emit(WsEvent::Message { id, data })).then_some((true, None))
        }
        // Pings are answered by the library; the app only speaks text.
        Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_))) => {
            None
        }
        Some(Ok(Message::Close(frame))) => Some((false, frame.map(|f| u16::from(f.code)))),
        Some(Err(_)) | None => Some((false, None)),
    }
}

/// Writes what the page queued before the connection is closed.
async fn flush_queued(ws: &mut Socket, rx: &mut mpsc::Receiver<String>) {
    while let Ok(text) = rx.try_recv() {
        if !write_text(ws, text).await {
            break;
        }
    }
}

async fn run(
    id: u64,
    instance: &Instance,
    dm: bool,
    mut rx: mpsc::Receiver<String>,
    close: &Notify,
    emit: &impl Fn(WsEvent) -> bool,
) -> Option<u16> {
    let Ok(mut ws) = connect(instance, dm).await else {
        return None;
    };
    if !emit(WsEvent::Open { id }) {
        return None;
    }
    // `goodbye` is true when this side ends the connection and should say
    // goodbye; `code` is the close code the instance ended it with.
    let (goodbye, code) = loop {
        tokio::select! {
            incoming = ws.next() => {
                if let Some(ending) = on_incoming(id, incoming, emit) {
                    break ending;
                }
            },
            outgoing = rx.recv() => match outgoing {
                Some(text) => {
                    if !write_text(&mut ws, text).await {
                        break (false, None);
                    }
                }
                None => break (true, None),
            },
            () = close.notified() => {
                // A browser socket writes what was queued before it closes.
                flush_queued(&mut ws, &mut rx).await;
                break (true, None);
            }
        }
    };
    // A peer that never answers the handshake is not waited for.
    if goodbye {
        let _ = tokio::time::timeout(WRITE_TIMEOUT, ws.close(None)).await;
    }
    code
}

#[cfg(test)]
mod tests;
