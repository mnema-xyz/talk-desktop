use super::*;
use crate::web_proxy::{Jar, Limits, client};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

async fn instance_with(listener: TcpListener, cookie: bool) -> Arc<Instance> {
    let origin = url::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let jar = Jar::new(None, 0);
    if cookie {
        jar.store(&["session=s3cret; Max-Age=3600".into()], false, now());
    }
    drop(listener);
    Arc::new(Instance {
        origin,
        client: client().unwrap(),
        jar,
        limits: Limits::default(),
    })
}

fn collector() -> (
    impl Fn(WsEvent) -> bool + Send + Sync + 'static,
    mpsc::UnboundedReceiver<WsEvent>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    (move |event| tx.send(event).is_ok(), rx)
}

async fn next(events: &mut mpsc::UnboundedReceiver<WsEvent>) -> WsEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("event in time")
        .expect("channel open")
}

/// The socket reaches an HTTPS instance on either protocol version, the way
/// the health check does (`web_tls`): a TLS 1.3-only server was out of
/// reach of the system stack on Windows 10.
async fn greeting_over(
    versions: &'static [&'static rustls::SupportedProtocolVersion],
) -> Option<String> {
    use crate::web_tls::tests::{Serves, serve};
    let origin = serve(versions, Serves::Socket, true).await;
    let instance = Instance {
        origin,
        client: client().unwrap(),
        jar: Jar::new(None, 0),
        limits: Limits::default(),
    };
    let mut ws = connect(&instance, false).await.ok()?;
    match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => Some(text.as_str().to_owned()),
        _ => None,
    }
}

#[tokio::test]
async fn the_socket_opens_to_a_server_that_offers_only_tls_1_3() {
    use crate::web_tls::tests::{GREETING, TLS13_ONLY};
    assert_eq!(greeting_over(TLS13_ONLY).await.as_deref(), Some(GREETING));
}

#[tokio::test]
async fn the_socket_opens_to_a_server_that_offers_only_tls_1_2() {
    use crate::web_tls::tests::{GREETING, TLS12_ONLY};
    assert_eq!(greeting_over(TLS12_ONLY).await.as_deref(), Some(GREETING));
}

#[tokio::test]
async fn the_socket_refuses_a_certificate_the_system_does_not_trust() {
    use crate::web_tls::tests::{Serves, TLS13_ONLY, serve};
    let origin = serve(TLS13_ONLY, Serves::Socket, false).await;
    let instance = Instance {
        origin,
        client: client().unwrap(),
        jar: Jar::new(None, 0),
        limits: Limits::default(),
    };
    assert!(connect(&instance, false).await.is_err());
}

#[test]
fn events_have_the_shape_the_app_reads() {
    assert_eq!(
        serde_json::to_string(&WsEvent::Open { id: 3 }).unwrap(),
        r#"{"kind":"open","id":3}"#
    );
    assert_eq!(
        serde_json::to_string(&WsEvent::Message {
            id: 3,
            data: "x".into()
        })
        .unwrap(),
        r#"{"kind":"message","id":3,"data":"x"}"#
    );
    assert_eq!(
        serde_json::to_string(&WsEvent::Close { id: 3, code: None }).unwrap(),
        r#"{"kind":"close","id":3}"#
    );
}

#[test]
fn the_page_learns_the_close_code_of_the_instance_and_nothing_else_about_it() {
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
    let emit = |_: WsEvent| true;
    let replaced = Message::Close(Some(CloseFrame {
        code: CloseCode::from(4001),
        reason: "replaced".into(),
    }));
    assert_eq!(
        on_incoming(1, Some(Ok(replaced)), &emit),
        Some((false, Some(4001)))
    );
    assert_eq!(
        on_incoming(1, Some(Ok(Message::Close(None))), &emit),
        Some((false, None))
    );
    assert_eq!(on_incoming(1, None, &emit), Some((false, None)));
    assert_eq!(
        serde_json::to_string(&WsEvent::Close {
            id: 3,
            code: Some(4001)
        })
        .unwrap(),
        r#"{"kind":"close","id":3,"code":4001}"#
    );
}

#[test]
fn a_page_may_ask_for_the_plain_socket_or_the_one_with_conversations_and_nothing_else() {
    assert_eq!(socket_request("/api/ws"), Ok(false));
    assert_eq!(socket_request("/api/ws?dm=1"), Ok(true));
    for refused in [
        "/api/ws?dm=0",
        "/api/ws?dm=1&x=1",
        "/api/ws?x=1",
        "/api/ws?",
        "/api/ws/",
        "/api/ws#x",
        "/api/other",
        "/api/ws?dm=1\n",
        "//evil.example/api/ws",
        "",
    ] {
        assert!(socket_request(refused).is_err(), "{refused:?}");
    }
}

#[tokio::test]
async fn the_conversation_question_reaches_the_server_only_when_asked() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let instance = instance_with(listener, false).await;
    assert_eq!(socket_url(&instance, false).unwrap().query(), None);
    assert_eq!(socket_url(&instance, true).unwrap().query(), Some("dm=1"));
    assert_eq!(socket_url(&instance, true).unwrap().path(), "/api/ws");
    assert_eq!(socket_url(&instance, true).unwrap().scheme(), "ws");
}

#[test]
fn unknown_sockets_and_oversized_messages_are_refused() {
    let sockets = Sockets::default();
    assert!(sockets.send(1, "x".into()).is_err());
    sockets.close(1);
    sockets.close_all();
    let (queue, _rx) = mpsc::channel(MAX_QUEUED);
    let close = Arc::new(Notify::new());
    sockets
        .open
        .lock()
        .unwrap()
        .insert(1, Handle { queue, close });
    assert!(sockets.send(1, "x".repeat(MAX_OUTGOING + 1)).is_err());
    assert_eq!(sockets.count(), 1);
    // The queue is bounded: a page that outruns the socket is told so.
    for _ in 0..MAX_QUEUED {
        assert!(sockets.send(1, "ok".into()).is_ok());
    }
    assert_eq!(sockets.send(1, "more".into()), Err("socket busy".into()));
}

#[tokio::test]
async fn texts_queued_before_a_close_are_written_first() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let instance = instance_with(listener, false).await;
    let server = TcpListener::bind(addr).await.unwrap();
    let (got_tx, got_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = server.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut texts = Vec::new();
        while let Some(Ok(message)) = ws.next().await {
            if let Message::Text(text) = message {
                texts.push(text.as_str().to_owned());
            }
        }
        let _ = got_tx.send(texts);
    });
    let sockets = Arc::new(Sockets::default());
    let (emit, mut events) = collector();
    let id = open(&sockets, Arc::clone(&instance), false, emit).unwrap();
    assert_eq!(next(&mut events).await, WsEvent::Open { id });
    for n in 0..5 {
        sockets.send(id, format!("m{n}")).unwrap();
    }
    sockets.close(id);
    let texts = tokio::time::timeout(Duration::from_secs(5), got_rx)
        .await
        .expect("server saw the end")
        .unwrap();
    assert_eq!(texts, ["m0", "m1", "m2", "m3", "m4"]);
}

#[tokio::test]
// The handshake callback returns tungstenite's own large error type.
#[allow(clippy::result_large_err)]
async fn the_host_connects_as_the_instance_would_see_a_browser_and_relays_text() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let instance = instance_with(listener, true).await;
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let server = TcpListener::bind(addr).await.unwrap();
    tokio::spawn(async move {
        let (stream, _) = server.accept().await.unwrap();
        let mut seen = Some(seen_tx);
        let mut ws =
            tokio_tungstenite::accept_hdr_async(stream, |req: &Request, resp: Response| {
                let get = |n: &str| req.headers().get(n).map(|v| v.to_str().unwrap().to_owned());
                let _ = seen.take().unwrap().send((
                    req.uri().path().to_owned(),
                    get("origin"),
                    get("cookie"),
                ));
                Ok(resp)
            })
            .await
            .unwrap();
        ws.send(Message::text("hello")).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            if let Message::Text(text) = message {
                ws.send(Message::text(format!("echo:{}", text.as_str())))
                    .await
                    .unwrap();
            }
        }
    });
    let sockets = Arc::new(Sockets::default());
    let (emit, mut events) = collector();
    let id = open(&sockets, Arc::clone(&instance), false, emit).unwrap();
    let (path, origin, cookie) = seen_rx.await.unwrap();
    assert_eq!(path, "/api/ws");
    assert_eq!(
        origin.as_deref(),
        Some(instance.origin.origin().ascii_serialization().as_str())
    );
    assert_eq!(cookie.as_deref(), Some("session=s3cret"));
    assert_eq!(next(&mut events).await, WsEvent::Open { id });
    assert_eq!(
        next(&mut events).await,
        WsEvent::Message {
            id,
            data: "hello".into()
        }
    );
    sockets.send(id, "ping".into()).unwrap();
    assert_eq!(
        next(&mut events).await,
        WsEvent::Message {
            id,
            data: "echo:ping".into()
        }
    );
    sockets.close(id);
    assert_eq!(next(&mut events).await, WsEvent::Close { id, code: None });
    assert!(
        events.recv().await.is_none(),
        "Close is the last event, sent once"
    );
    assert_eq!(sockets.count(), 0);
}

#[tokio::test]
// The handshake callback returns tungstenite's own large error type.
#[allow(clippy::result_large_err)]
async fn the_conversation_question_is_sent_to_the_server_when_the_page_asked_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let instance = instance_with(listener, false).await;
    let server = TcpListener::bind(addr).await.unwrap();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = server.accept().await.unwrap();
        let mut seen = Some(seen_tx);
        let _ws = tokio_tungstenite::accept_hdr_async(stream, |req: &Request, resp: Response| {
            let _ = seen.take().unwrap().send(req.uri().to_string());
            Ok(resp)
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let sockets = Arc::new(Sockets::default());
    let (emit, _events) = collector();
    let id = open(&sockets, instance, true, emit).unwrap();
    assert_eq!(seen_rx.await.unwrap(), "/api/ws?dm=1");
    sockets.close(id);
}

#[tokio::test]
async fn an_unreachable_instance_closes_the_socket_without_opening_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let instance = instance_with(listener, false).await;
    let sockets = Arc::new(Sockets::default());
    let (emit, mut events) = collector();
    let id = open(&sockets, instance, false, emit).unwrap();
    assert_eq!(next(&mut events).await, WsEvent::Close { id, code: None });
    assert_eq!(sockets.count(), 0);
}

#[tokio::test]
async fn the_number_of_sockets_is_bounded_and_closing_all_ends_them() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let instance = instance_with(listener, false).await;
    let server = TcpListener::bind(addr).await.unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = server.accept().await.unwrap();
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while ws.next().await.is_some() {}
            });
        }
    });
    let sockets = Arc::new(Sockets::default());
    let (emit, mut events) = collector();
    let emit = Arc::new(emit);
    for _ in 0..MAX_SOCKETS {
        let emit = Arc::clone(&emit);
        open(&sockets, Arc::clone(&instance), false, move |e| emit(e)).unwrap();
    }
    assert!(open(&sockets, Arc::clone(&instance), false, |_| true).is_err());
    let mut opened = 0;
    while opened < MAX_SOCKETS {
        if matches!(next(&mut events).await, WsEvent::Open { .. }) {
            opened += 1;
        }
    }
    sockets.close_all();
    let mut closed = 0;
    while closed < MAX_SOCKETS {
        if matches!(next(&mut events).await, WsEvent::Close { .. }) {
            closed += 1;
        }
    }
}

/// A server that accepts WebSockets and then never reads or answers again.
async fn stalled_server(listener: TcpListener) {
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let _ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
        }
    });
}

#[tokio::test]
async fn a_reloaded_page_frees_the_slots_of_the_one_before() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let instance = instance_with(listener, false).await;
    stalled_server(TcpListener::bind(addr).await.unwrap()).await;
    let sockets = Arc::new(Sockets::default());
    let (emit, mut events) = collector();
    let emit = Arc::new(emit);
    for _ in 0..MAX_SOCKETS {
        let emit = Arc::clone(&emit);
        open(&sockets, Arc::clone(&instance), false, move |e| emit(e)).unwrap();
    }
    assert!(open(&sockets, Arc::clone(&instance), false, |_| true).is_err());
    let mut opened = 0;
    while opened < MAX_SOCKETS {
        if matches!(next(&mut events).await, WsEvent::Open { .. }) {
            opened += 1;
        }
    }
    // The page reloads: its sockets are gone and the new page can open its own.
    sockets.document_replaced();
    assert_eq!(sockets.count(), 0);
    assert!(open(&sockets, Arc::clone(&instance), false, |_| true).is_ok());
    let mut closed = 0;
    while closed < MAX_SOCKETS {
        if matches!(next(&mut events).await, WsEvent::Close { .. }) {
            closed += 1;
        }
    }
}

#[tokio::test]
async fn a_peer_that_stops_reading_does_not_hold_the_socket_for_ever() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let instance = instance_with(listener, false).await;
    stalled_server(TcpListener::bind(addr).await.unwrap()).await;
    let sockets = Arc::new(Sockets::default());
    let (emit, mut events) = collector();
    let id = open(&sockets, instance, false, emit).unwrap();
    assert_eq!(next(&mut events).await, WsEvent::Open { id });
    // Fill the connection until writes stall, then ask to close; the
    // handshake is never answered and the timeouts let go.
    for _ in 0..MAX_QUEUED {
        if sockets.send(id, "x".repeat(MAX_OUTGOING)).is_err() {
            break;
        }
    }
    sockets.close(id);
    assert_eq!(next(&mut events).await, WsEvent::Close { id, code: None });
    assert_eq!(sockets.count(), 0);
}

/// Against a real server (MNEMA_TEST_INSTANCE, MNEMA_TEST_USER,
/// MNEMA_TEST_PASSWORD): after a login through the proxy the instance
/// accepts the host's socket and sends its events.
#[tokio::test]
async fn the_real_server_accepts_the_hosts_socket_after_login() {
    let (Ok(origin), Ok(user), Ok(password)) = (
        std::env::var("MNEMA_TEST_INSTANCE"),
        std::env::var("MNEMA_TEST_USER"),
        std::env::var("MNEMA_TEST_PASSWORD"),
    ) else {
        return;
    };
    let instance = Arc::new(Instance {
        origin: url::Url::parse(&origin).unwrap(),
        client: client().unwrap(),
        jar: Jar::new(None, 0),
        limits: Limits::default(),
    });
    let json = [("content-type".to_owned(), "application/json".to_owned())];
    let body = serde_json::json!({"username": user, "password": password})
        .to_string()
        .into_bytes();
    assert_eq!(
        crate::web_proxy::forward(&instance, "POST", "/api/auth/login", &json, body)
            .await
            .status,
        200
    );
    let sockets = Arc::new(Sockets::default());
    let (emit, mut events) = collector();
    let id = open(&sockets, instance, false, emit).unwrap();
    assert_eq!(next(&mut events).await, WsEvent::Open { id });
    let WsEvent::Message { data, .. } = next(&mut events).await else {
        panic!("expected a message")
    };
    assert!(data.contains("\"type\""), "{data}");
    sockets.close(id);
    while next(&mut events).await != (WsEvent::Close { id, code: None }) {}
}
