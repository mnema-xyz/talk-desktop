use super::*;
use base64::Engine;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn b64(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap()
}

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../../test-vectors/rail-icon.json")).unwrap()
}

/// A valid png with a text chunk of the size that makes the whole file
/// `total` bytes long.
fn png_of(total: usize) -> Vec<u8> {
    let file = vectors();
    let base = b64(file["cases"][0]["body"].as_str().unwrap());
    let (head, end) = base.split_at(base.len() - 12);
    let text = vec![b'x'; total - base.len() - 12];
    let mut out = head.to_vec();
    out.extend((text.len() as u32).to_be_bytes());
    out.extend(b"tEXt");
    out.extend(&text);
    out.extend(crc32(&[b"tEXt", &text]).to_be_bytes());
    out.extend(end);
    out
}

#[test]
fn the_shared_vectors_give_the_answers_written_in_them() {
    let file = vectors();
    assert_eq!(file["limit"], LIMIT);
    assert_eq!(file["max_side"], MAX_SIDE);
    for case in file["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let got = validate(
            case["content_type"].as_str(),
            &b64(case["body"].as_str().unwrap()),
        );
        if case["ok"].as_bool().unwrap() {
            let kind = if case["kind"] == "png" {
                Kind::Png
            } else {
                Kind::Webp
            };
            let size = (
                case["width"].as_u64().unwrap() as u32,
                case["height"].as_u64().unwrap() as u32,
            );
            assert_eq!(
                got.map(|(k, w, h)| (k, (w, h))),
                Some((kind, size)),
                "{name}"
            );
        } else {
            assert_eq!(got, None, "{name}");
        }
    }
}

#[test]
fn one_mebibyte_is_the_most() {
    assert_eq!(png_of(LIMIT).len(), LIMIT);
    assert!(validate(Some("image/png"), &png_of(LIMIT)).is_some());
    assert_eq!(validate(Some("image/png"), &png_of(LIMIT + 1)), None);
}

#[test]
fn the_health_answer_decides_whether_to_ask() {
    assert!(wanted(br#"{"community_icon":true}"#));
    assert!(wanted(br#"{"community_icon":"v7"}"#));
    assert!(wanted(br#"{"version":"1.0.0"}"#));
    assert!(!wanted(br#"{"community_icon":false}"#));
    assert!(!wanted(br#"{"community_icon":""}"#));
    assert!(!wanted(br#"{"community_icon":null}"#));
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mnema-icon-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn the_cache_keeps_one_icon_per_server_and_checks_it_again_when_read() {
    let dir = scratch("cache");
    let id = "0123456789abcdef";
    let png = b64(vectors()["cases"][0]["body"].as_str().unwrap());
    let webp = b64(vectors()["cases"][2]["body"].as_str().unwrap());
    store(&dir, id, Kind::Png, &png, Some("\"v1\"")).unwrap();
    assert_eq!(read(&dir, id), Some((Kind::Png, png.clone())));
    assert_eq!(cached_etag(&dir, id).as_deref(), Some("\"v1\""));
    store(&dir, id, Kind::Webp, &webp, None).unwrap();
    assert_eq!(read(&dir, id).map(|(k, _)| k), Some(Kind::Webp));
    assert!(!file(&dir, id, Kind::Png).exists());
    assert_eq!(cached_etag(&dir, id), None);
    // A file that was changed on disk is not shown.
    std::fs::write(file(&dir, id, Kind::Webp), b"<svg/>").unwrap();
    assert_eq!(read(&dir, id), None);
    // Names that are not server ids never reach the disk.
    store(&dir, "../x", Kind::Png, &png, None).unwrap();
    assert_eq!(read(&dir, "../x"), None);
    store(&dir, id, Kind::Png, &png, Some("\"v2\"")).unwrap();
    store(&dir, "fedcba9876543210", Kind::Png, &png, None).unwrap();
    sweep(&dir, &[id.to_owned()]);
    assert!(read(&dir, "fedcba9876543210").is_none());
    assert!(read(&dir, id).is_some());
    clear(&dir, id);
    assert!(read(&dir, id).is_none() && !etag_file(&dir, id).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_rail_gets_a_data_url_and_a_name_that_changes_with_the_icon() {
    let png = b64(vectors()["cases"][0]["body"].as_str().unwrap());
    assert!(data_url(Kind::Png, &png).starts_with("data:image/png;base64,iVBOR"));
    assert_eq!(token(&png).len(), 16);
    assert_ne!(token(&png), token(&png_of(LIMIT)));
}

/// Answers each connection with the next canned response, and keeps what
/// the request said.
async fn serve(answers: Vec<Vec<u8>>) -> (Url, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://{}/api/community/icon",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let task = tokio::spawn(async move {
        let mut seen = Vec::new();
        for answer in answers {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            seen.push(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase());
            let _ = socket.write_all(&answer).await;
            let _ = socket.shutdown().await;
        }
        seen
    });
    (url, task)
}

fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nconnection: close\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    out.extend(body);
    out
}

#[tokio::test]
async fn the_icon_is_asked_for_from_the_server_with_its_validator_and_judged_by_what_comes() {
    let png = b64(vectors()["cases"][0]["body"].as_str().unwrap());
    let svg: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
    let (url, task) = serve(vec![
        response(
            "200 OK",
            &[("content-type", "image/png"), ("etag", "\"v1\"")],
            &png,
        ),
        response("304 Not Modified", &[], b""),
        response("404 Not Found", &[], b""),
        response("200 OK", &[("content-type", "image/svg+xml")], svg),
        response(
            "302 Found",
            &[("location", "http://127.0.0.1:9/x.png")],
            b"",
        ),
        response("500 Oops", &[], b""),
        response(
            "200 OK",
            &[("content-type", "image/png")],
            &png_of(LIMIT + 1),
        ),
    ])
    .await;
    assert_eq!(
        fetch_url(url.clone(), None).await,
        Outcome::Fresh(Kind::Png, png, Some("\"v1\"".into()))
    );
    assert_eq!(
        fetch_url(url.clone(), Some("\"v1\"")).await,
        Outcome::Unchanged
    );
    assert_eq!(fetch_url(url.clone(), None).await, Outcome::Gone);
    assert_eq!(fetch_url(url.clone(), None).await, Outcome::Failed);
    assert_eq!(fetch_url(url.clone(), None).await, Outcome::Failed);
    assert_eq!(fetch_url(url.clone(), None).await, Outcome::Failed);
    assert_eq!(fetch_url(url, None).await, Outcome::Failed);
    let seen = task.await.unwrap();
    assert!(seen[0].starts_with("get /api/community/icon "));
    assert!(!seen[0].contains("if-none-match"));
    assert!(seen[1].contains("if-none-match: \"v1\""));
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_cache_and_a_missing_icon_clears_it() {
    let dir = scratch("refresh");
    let id = "0123456789abcdef";
    let png = b64(vectors()["cases"][0]["body"].as_str().unwrap());
    store(&dir, id, Kind::Png, &png, None).unwrap();
    let (url, _task) = serve(vec![
        response("500 Oops", &[], b""),
        response("404 Not Found", &[], b""),
    ])
    .await;
    let origin = url.join("/").unwrap();
    assert!(!refresh(&dir, id, &origin, true).await);
    assert!(read(&dir, id).is_some());
    assert!(refresh(&dir, id, &origin, true).await);
    assert!(read(&dir, id).is_none());
    // Off in the health answer: nothing is asked.
    store(&dir, id, Kind::Png, &png, None).unwrap();
    assert!(refresh(&dir, id, &origin, false).await);
    assert!(read(&dir, id).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
