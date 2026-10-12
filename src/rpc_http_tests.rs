use std::time::Duration;

use bitcoin::hashes::{Hash, sha256};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use crate::guardian_rpc::Pending;
use crate::rpc_method::{RpcError, RpcRequest};

fn request() -> RpcRequest {
    RpcRequest::parse(json!({"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}))
        .ok()
        .unwrap()
}

async fn connection(
    send: mpsc::Sender<Pending>,
) -> (TcpStream, tokio::task::JoinHandle<std::io::Result<()>>) {
    let (client, server) = streams().await;
    let auth = sha256::Hash::hash(b"Basic dTpw").to_byte_array();
    let (slow, _receive) = mpsc::channel(1);
    let task = tokio::spawn(crate::rpc_http::serve(server, auth, send, slow));
    (client, task)
}

async fn streams() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    (client, server)
}

async fn write(client: &mut TcpStream, body: &str) {
    client
        .write_all(
            format!(
                "POST / HTTP/1.1\r\nAuthorization: Basic dTpw\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn saturated_queue_rejects_instead_of_spawning_unbounded_jobs() {
    let (send, _receive) = mpsc::channel(4);
    let mut waits = Vec::new();
    for _ in 0..4 {
        let (reply, wait) = oneshot::channel();
        send.try_send(Pending {
            request: request(),
            reply,
        })
        .ok()
        .unwrap();
        waits.push(wait);
    }
    let (mut client, task) = connection(send).await;
    write(
        &mut client,
        r#"{"jsonrpc":"2.0","id":1,"method":"getblockcount"}"#,
    )
    .await;
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.contains("-32014"), "{response}");
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn disconnect_cancels_waiting_owner_reply_without_detaching() {
    let (send, mut receive) = mpsc::channel(4);
    let (mut client, task) = connection(send).await;
    write(
        &mut client,
        r#"{"jsonrpc":"2.0","id":1,"method":"getblockcount"}"#,
    )
    .await;
    let mut pending = receive.recv().await.unwrap();
    drop(client);
    tokio::time::timeout(Duration::from_secs(1), pending.reply.closed())
        .await
        .unwrap();
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn structural_json_limit_rejects_before_owner_dispatch() {
    let (send, mut receive) = mpsc::channel(4);
    let (mut client, task) = connection(send).await;
    write(&mut client, &format!("[{}]", "[],".repeat(100))).await;
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.contains("-32700"));
    assert!(receive.try_recv().is_err());
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn terminal_owner_failure_drains_error_before_aborting_http_tasks() {
    let (send, mut receive) = mpsc::channel(4);
    let (mut client, server) = streams().await;
    let auth = sha256::Hash::hash(b"Basic dTpw").to_byte_array();
    let (slow, _receive) = mpsc::channel(1);
    let mut connections = tokio::task::JoinSet::new();
    connections.spawn(async move {
        crate::rpc_http::serve(server, auth, send, slow)
            .await
            .unwrap();
    });
    write(
        &mut client,
        r#"{"jsonrpc":"2.0","id":1,"method":"getblockcount"}"#,
    )
    .await;
    let pending = receive.recv().await.unwrap();
    pending
        .reply
        .send(Err(RpcError::new(-32013, "local state failure")))
        .ok()
        .unwrap();
    // Same current-thread ordering as the CLI: no yield between send and shutdown.
    crate::guardian_rpc::finish_connections(&mut connections, true).await;
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.contains("-32013"), "{response}");
    assert!(connections.is_empty());
}
