//! Offline real-header and loopback HTTP fixtures, never public explorer polling.

use std::sync::{Arc, Mutex};

use bitcoin::block::Header;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hex::DisplayHex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub(crate) fn headers() -> Vec<Header> {
    include_bytes!("../tests/data/mainnet-headers-0-2035.raw")
        .chunks_exact(80)
        .map(|bytes| deserialize(bytes).unwrap())
        .collect()
}

pub(crate) fn reply(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Loopback server task is aborted on drop, including failed assertions.
pub(crate) struct Server {
    pub root: String,
    pub paths: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub async fn new(response: impl Fn(&str) -> Vec<u8> + Send + 'static) -> Self {
        Self::delayed(response, std::time::Duration::ZERO).await
    }

    pub async fn delayed(
        response: impl Fn(&str) -> Vec<u8> + Send + 'static,
        delay: std::time::Duration,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let paths = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&paths);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let Ok(byte) = stream.read_u8().await else {
                        break;
                    };
                    request.push(byte);
                    assert!(request.len() < 8192);
                }
                if !request.ends_with(b"\r\n\r\n") {
                    continue;
                }
                let request = String::from_utf8(request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap();
                recorded.lock().unwrap().push(path.into());
                tokio::time::sleep(delay).await;
                let _ = stream.write_all(&response(path)).await;
            }
        });
        Self { root, paths, task }
    }

    pub async fn chain(tip: usize) -> Self {
        Self::delayed_chain(tip, std::time::Duration::ZERO).await
    }

    pub async fn delayed_chain(tip: usize, delay: std::time::Duration) -> Self {
        let headers = headers();
        Self::delayed(
            move |path| {
                if path == "/blocks/tip/hash" {
                    return reply(&format!("{}\n", headers[tip].block_hash()));
                }
                for header in &headers[..=tip] {
                    if path == format!("/block/{}/header", header.block_hash()) {
                        return reply(&format!("{}\n", serialize(header).to_lower_hex_string()));
                    }
                }
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec()
            },
            delay,
        )
        .await
    }
}

pub(crate) async fn p2p_read(
    stream: &mut tokio::net::TcpStream,
) -> Option<bitcoin::p2p::message::NetworkMessage> {
    let mut envelope = [0; 24];
    stream.read_exact(&mut envelope).await.ok()?;
    let len = u32::from_le_bytes(envelope[16..20].try_into().unwrap()) as usize;
    assert!(len < 2_000_000);
    let mut frame = envelope.to_vec();
    frame.resize(24 + len, 0);
    stream.read_exact(&mut frame[24..]).await.ok()?;
    Some(
        deserialize::<bitcoin::p2p::message::RawNetworkMessage>(&frame)
            .unwrap()
            .into_payload(),
    )
}

pub(crate) async fn p2p_send(
    stream: &mut tokio::net::TcpStream,
    message: bitcoin::p2p::message::NetworkMessage,
) {
    stream
        .write_all(&serialize(&bitcoin::p2p::message::RawNetworkMessage::new(
            bitcoin::Network::Bitcoin.magic(),
            message,
        )))
        .await
        .unwrap();
}

/// Advertises an arbitrary height but never supplies any headers.
pub(crate) async fn withholding_peer() -> (std::net::SocketAddr, JoinHandle<usize>) {
    use bitcoin::p2p::message::NetworkMessage;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut stream = p2p_handshake(&listener).await;
        let mut requests = 0;
        while let Some(message) = p2p_read(&mut stream).await {
            match message {
                NetworkMessage::GetHeaders(_) => {
                    requests += 1;
                    p2p_send(&mut stream, NetworkMessage::Headers(vec![])).await;
                }
                NetworkMessage::Ping(nonce) => {
                    p2p_send(&mut stream, NetworkMessage::Pong(nonce)).await
                }
                _ => {}
            }
        }
        requests
    });
    (address, task)
}

pub(crate) async fn p2p_handshake(listener: &TcpListener) -> tokio::net::TcpStream {
    use bitcoin::p2p::{
        ServiceFlags, address::Address, message::NetworkMessage, message_network::VersionMessage,
    };
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
        p2p_read(&mut stream).await,
        Some(NetworkMessage::Version(_))
    ));
    let addr = Address::new(&listener.local_addr().unwrap(), ServiceFlags::NETWORK);
    p2p_send(
        &mut stream,
        NetworkMessage::Version(VersionMessage::new(
            ServiceFlags::NETWORK,
            0,
            addr.clone(),
            addr,
            123,
            "/withholding/".into(),
            i32::MAX,
        )),
    )
    .await;
    p2p_send(&mut stream, NetworkMessage::Verack).await;
    assert!(matches!(
        p2p_read(&mut stream).await,
        Some(NetworkMessage::Verack)
    ));
    stream
}

/// Advance only between completed network exchanges; paused I/O time auto-advance
/// would otherwise race the intentionally short real HTTP deadline.
pub(crate) async fn elapse(duration: std::time::Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}
