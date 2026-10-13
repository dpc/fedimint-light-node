//! Strict single-request HTTP framing keeps connection, parser and copy budgets local.

use std::io;
use std::time::Duration;

use bitcoin::hashes::{Hash, sha256};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::guardian_rpc::Pending;
use crate::rpc_method::{RpcError, RpcRequest};

/// Hex of the largest admitted transaction plus a small JSON envelope.
const MAX_BODY: usize = 801_024;
/// Complete raw block hex plus a small JSON envelope.
const MAX_REPLY: usize = 8_001_024;

pub(crate) async fn serve(
    mut stream: TcpStream,
    authorization: Option<[u8; 32]>,
    sender: mpsc::Sender<Pending>,
    slow_sender: mpsc::Sender<Pending>,
) -> io::Result<()> {
    let body = match timeout(
        Duration::from_secs(2),
        read_request(&mut stream, authorization),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(status)) => return write(&mut stream, status, b"").await,
        Err(_) => return write(&mut stream, "408 Request Timeout", b"").await,
    };
    let value = if bounded_json(&body) {
        serde_json::from_slice::<Value>(&body).ok()
    } else {
        None
    };
    drop(body);
    let (id, legacy, result) = match value {
        None => (
            Value::Null,
            false,
            Err(RpcError::new(
                -32700,
                "parse error or JSON complexity limit",
            )),
        ),
        Some(value) => {
            let id = value
                .get("id")
                .filter(|id| {
                    id.is_null()
                        || id.as_i64().is_some()
                        || id.as_u64().is_some()
                        || id.as_str().is_some_and(|s| s.len() <= 64)
                })
                .cloned()
                .unwrap_or(Value::Null);
            let legacy = value
                .get("jsonrpc")
                .is_none_or(|v| v == "1.0" || v.is_null());
            let request = RpcRequest::parse(value);
            let result = match request {
                Err(e) => Err(e),
                Ok(request) => {
                    #[cfg(all(test, target_os = "linux"))]
                    let network_request = request.is_network();
                    let (reply, receive) = oneshot::channel();
                    let sender = if request.is_network() {
                        &slow_sender
                    } else {
                        &sender
                    };
                    match sender.try_send(Pending { request, reply }) {
                        Err(_) => Err(RpcError::new(
                            -32014,
                            "busy; bounded operation queue is full",
                        )),
                        Ok(()) => {
                            #[cfg(all(test, target_os = "linux"))]
                            crate::readiness_observer::queue(network_request, true);
                            let mut extra = [0u8; 1];
                            tokio::select! {
                                result = timeout(Duration::from_secs(11), receive) => match result {
                                    Ok(Ok(result)) => result,
                                    _ => Err(RpcError::unavailable()),
                                },
                                // No pipelining or request-side half-close. Dropping the
                                // receiver notifies the owner and cancels its leased socket.
                                _ = stream.read(&mut extra) => {
                                    return Err(io::Error::other("RPC client disconnected or pipelined"));
                                }
                            }
                        }
                    }
                }
            };
            (id, legacy, result)
        }
    };
    let response = match (legacy, result) {
        (true, Ok(value)) => json!({"id": id, "result": value, "error": null}),
        (true, Err(error)) => json!({"id": id, "result": null, "error": error.value()}),
        (false, Ok(value)) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        (false, Err(error)) => json!({"jsonrpc": "2.0", "id": id, "error": error.value()}),
    };
    let encoded = serde_json::to_vec(&response)?;
    if encoded.len() > MAX_REPLY {
        return Err(io::Error::other("RPC reply exceeds bound"));
    }
    write(&mut stream, "200 OK", &encoded).await
}

/// Bound structural allocation before serde sees attacker-controlled JSON.
fn bounded_json(bytes: &[u8]) -> bool {
    let (mut quoted, mut escaped, mut depth, mut punctuation) = (false, false, 0u32, 0u32);
    for &byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'[' | b'{' => {
                    depth += 1;
                    punctuation += 1;
                }
                b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                }
                b',' | b':' => punctuation += 1,
                _ => {}
            }
            if depth > 8 || punctuation > 64 {
                return false;
            }
        }
    }
    true
}

async fn read_request(
    stream: &mut TcpStream,
    authorization: Option<[u8; 32]>,
) -> Result<Vec<u8>, &'static str> {
    let mut bytes = [0u8; 4096];
    let mut filled = 0;
    let end = loop {
        if filled == bytes.len() {
            return Err("431 Request Header Fields Too Large");
        }
        let n = stream
            .read(&mut bytes[filled..])
            .await
            .map_err(|_| "400 Bad Request")?;
        if n == 0 {
            return Err("400 Bad Request");
        }
        filled += n;
        if let Some(end) = bytes[..filled].windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let mut headers = [httparse::EMPTY_HEADER; 24];
    let mut request = httparse::Request::new(&mut headers);
    request
        .parse(&bytes[..end])
        .map_err(|_| "400 Bad Request")?;
    if request.method != Some("POST") || request.path != Some("/") || request.version != Some(1) {
        return Err("400 Bad Request");
    }
    let mut auth = None;
    let mut length = None;
    let mut host = None;
    let mut content_type = None;
    for header in request.headers {
        if header.name.eq_ignore_ascii_case("authorization") {
            if auth.replace(header.value).is_some() {
                return Err("400 Bad Request");
            }
        } else if authorization.is_none() && header.name.eq_ignore_ascii_case("origin") {
            // Native-only endpoint: reject browser requests, including Origin: null.
            return Err("403 Forbidden");
        } else if authorization.is_none() && header.name.eq_ignore_ascii_case("host") {
            if host.replace(header.value).is_some() {
                return Err("400 Bad Request");
            }
        } else if authorization.is_none() && header.name.eq_ignore_ascii_case("content-type") {
            if content_type.replace(header.value).is_some() {
                return Err("400 Bad Request");
            }
        } else if header.name.eq_ignore_ascii_case("content-length") {
            if length.is_some()
                || header.value.is_empty()
                || !header.value.iter().all(u8::is_ascii_digit)
            {
                return Err("400 Bad Request");
            }
            length = Some(
                std::str::from_utf8(header.value)
                    .map_err(|_| "400 Bad Request")?
                    .parse::<usize>()
                    .map_err(|_| "413 Content Too Large")?,
            );
        } else if ["transfer-encoding", "content-encoding", "expect"]
            .iter()
            .any(|name| header.name.eq_ignore_ascii_case(name))
        {
            return Err("400 Bad Request");
        }
    }
    if let Some(authorization) = authorization {
        let supplied = sha256::Hash::hash(auth.unwrap_or_default()).to_byte_array();
        if !bool::from(supplied.ct_eq(&authorization)) {
            return Err("401 Unauthorized");
        }
    } else {
        // Exact local authority prevents DNS rebinding; JSON excludes browser
        // simple form/text submissions. No CORS preflight or origins are accepted.
        let local = stream.local_addr().map_err(|_| "400 Bad Request")?;
        let numeric = local.to_string();
        // bitcoin-cli sends a bare numeric Host even for a non-default port.
        let bare_ip = local.ip().to_string();
        let bracketed_ip = format!("[{}]", local.ip());
        let localhost = format!("localhost:{}", local.port());
        if !host.is_some_and(|host| {
            host == numeric.as_bytes()
                || host == bare_ip.as_bytes()
                || (local.is_ipv6() && host == bracketed_ip.as_bytes())
                || host == localhost.as_bytes()
                || host == b"localhost"
        }) {
            return Err("403 Forbidden");
        }
        if !content_type.is_some_and(|value| value.eq_ignore_ascii_case(b"application/json")) {
            return Err("415 Unsupported Media Type");
        }
    }
    let length = length.ok_or("411 Length Required")?;
    if length > MAX_BODY {
        return Err("413 Content Too Large");
    }
    if filled - end > length {
        return Err("400 Bad Request");
    }
    let mut body = vec![0; length];
    body[..filled - end].copy_from_slice(&bytes[end..filled]);
    stream
        .read_exact(&mut body[filled - end..])
        .await
        .map_err(|_| "400 Bad Request")?;
    Ok(body)
}

async fn write(stream: &mut TcpStream, status: &str, body: &[u8]) -> io::Result<()> {
    #[cfg(all(test, target_os = "linux"))]
    let _writer = crate::readiness_observer::enter(crate::readiness_observer::Counter::Writer);
    let challenge = if status == "401 Unauthorized" {
        "WWW-Authenticate: Basic realm=\"guardian\"\r\n"
    } else {
        ""
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{challenge}\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    #[cfg(not(all(test, target_os = "linux")))]
    stream.write_all(body).await?;
    #[cfg(all(test, target_os = "linux"))]
    {
        use std::future::Future;
        use std::task::Poll;

        let mut write = std::pin::pin!(stream.write_all(body));
        let mut blocked = None;
        std::future::poll_fn(|cx| match write.as_mut().poll(cx) {
            Poll::Pending => {
                if blocked.is_none() {
                    blocked = crate::readiness_observer::enter(
                        crate::readiness_observer::Counter::BlockedWriter,
                    );
                }
                Poll::Pending
            }
            ready => {
                blocked.take();
                ready
            }
        })
        .await?;
    }
    stream.shutdown().await
}
