use super::*;
use crate::explorer_test_support::{Server, reply};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn configuration_rejects_unsafe_urls_and_duplicate_origins() {
    for roots in [
        vec!["http://example.org/api"],
        vec!["https://user:secret@example.org/api"],
        vec!["https://example.org/api?secret"],
        vec!["https://example.org/api#fragment"],
        vec!["https://example.org/a", "https://example.org:443/b"],
        vec!["https://example.org"; 9],
    ] {
        assert!(
            ExplorerHttp::new(&ExplorerHeaderConfig {
                roots: roots.into_iter().map(String::from).collect(),
            })
            .is_err()
        );
    }
    assert!(ExplorerHeaderConfig::public_defaults().validate().is_ok());
    assert!(
        ExplorerHeaderConfig {
            roots: vec![format!("https://example.org/{}", "x".repeat(2048))]
        }
        .validate()
        .is_err()
    );
}

#[test]
fn retry_after_seconds_dates_invalid_and_excessive_inputs() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for (input, expected) in [
        ("0", Some(0)),
        ("3600", Some(3600)),
        ("-1", Some(0)),
        ("nonsense", Some(0)),
        ("604800", Some(604800)),
        ("604801", None),
        ("999999999999999999999999999999", None),
    ] {
        assert_eq!(retry_after(input, now), expected.map(Duration::from_secs));
    }
    let future = httpdate::fmt_http_date(now + Duration::from_secs(5000));
    assert_eq!(retry_after(&future, now), Some(Duration::from_secs(5000)));
    assert_eq!(
        retry_after(&httpdate::fmt_http_date(now - Duration::from_secs(10)), now),
        Some(Duration::ZERO)
    );
    assert_eq!(retry_after(&"9".repeat(200), now), None);
    assert_eq!(retry_after(&"bad".repeat(200), now), None);
}

#[tokio::test]
async fn rejects_redirects_encoding_and_oversized_chunked_or_sized_bodies() {
    for raw in [
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Encoding: identity\r\nContent-Length: 0\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 9999999999\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nabcdef\r\n0\r\n\r\n".to_vec(),
        reply("123456"),
    ] {
        let server = Server::new(move |_| raw.clone()).await;
        let client = ExplorerHttp::new(&ExplorerHeaderConfig {
            roots: vec![server.root.clone()],
        })
        .unwrap();
        assert!(matches!(
            client.text(0, "blocks/tip/hash", 5).await,
            Err(FetchError::Reply)
        ));
        assert_eq!(server.paths.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn retry_after_status_does_not_buffer_error_body_or_retry() {
    for status in [429, 503] {
        let server = Server::new(move |_| {
            format!(
                "HTTP/1.1 {status} Busy\r\nRetry-After: 3600\r\nContent-Length: 999999999\r\n\r\n"
            )
            .into_bytes()
        })
        .await;
        let client = ExplorerHttp::new(&ExplorerHeaderConfig {
            roots: vec![server.root.clone()],
        })
        .unwrap();
        assert!(matches!(client.text(0, "blocks/tip/hash", 65).await,
            Err(FetchError::RateLimited(Some(delay))) if delay == Duration::from_secs(3600)));
        assert_eq!(server.paths.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn chunk_trickle_has_whole_request_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let serving = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        for _ in 0..10 {
            if stream.write_all(b"1\r\nx\r\n").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let client = ExplorerHttp::new(&ExplorerHeaderConfig { roots: vec![root] }).unwrap();
    let start = Instant::now();
    assert!(matches!(
        client.text(0, "blocks/tip/hash", 65).await,
        Err(FetchError::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_secs(5));
    serving.abort();
}

use tokio::time::Instant;
