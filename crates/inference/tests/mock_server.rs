//! The client against a local HTTP server that replays canned responses.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use vivus_inference::{ChatRequest, Client, Error, Event, Message};

/// Serves one connection: returns the request text, then writes `parts` with a short pause each.
async fn serve(
    head: &'static str,
    parts: Vec<String>,
) -> (Client, oneshot::Receiver<(String, TcpStream)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/", listener.local_addr().unwrap());
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let req = read_request(&mut sock).await;
        sock.write_all(head.as_bytes()).await.unwrap();
        for p in parts {
            if sock.write_all(p.as_bytes()).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let _ = sock.shutdown().await;
        let _ = tx.send((req, sock));
    });
    (Client::new(url, Some("sk-test".into())), rx)
}

async fn read_request(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = sock.read(&mut tmp).await.unwrap();
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(end) = text.find("\r\n\r\n") {
            let len: usize = text
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                return text.into_owned();
            }
        }
    }
}

const SSE: &str = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";

fn sse(json: &str) -> String {
    format!("data: {json}\n\n")
}

fn request() -> ChatRequest {
    ChatRequest::new("test-model", vec![Message::User("hi".into())])
}

#[tokio::test]
async fn streams_text_and_tool_calls() {
    let parts = vec![
        ": OPENROUTER PROCESSING\n\n".into(),
        sse(r#"{"choices":[{"delta":{"content":"He"}}]}"#),
        sse(r#"{"choices":[{"delta":{"content":"llo"}}]}"#),
        sse(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"speak","arguments":"{\"text\":\"Hi"}}]}}]}"#),
        sse(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"}"}}]},"finish_reason":"tool_calls"}]}"#),
        "data: [DONE]\n\n".into(),
    ];
    let (client, rx) = serve(SSE, parts).await;
    let mut stream = client.chat(&request()).await.unwrap();
    let mut text = String::new();
    let mut done = None;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            Event::Text(t) => text.push_str(&t),
            Event::Done(r) => done = Some(r),
            _ => {}
        }
    }
    assert_eq!(text, "Hello");
    let reply = done.unwrap();
    assert_eq!(reply.content, "Hello");
    assert_eq!(reply.tool_calls[0].arguments, r#"{"text":"Hi"}"#);

    let (req, _) = rx.await.unwrap();
    assert!(req.starts_with("POST /v1/chat/completions"));
    assert!(req.to_ascii_lowercase().contains("authorization: bearer sk-test"));
    assert!(req.contains(r#""stream":true"#));
}

#[tokio::test]
async fn json_body_instead_of_stream() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}]}"#;
    let (client, _rx) = serve(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
        vec![body.into()],
    )
    .await;
    let reply = client.complete(&request()).await.unwrap();
    assert_eq!(reply.content, "Hi");
}

#[tokio::test]
async fn http_and_in_stream_errors() {
    let (client, _rx) = serve(
        "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
        vec![r#"{"error":{"message":"rate limited","type":"x"}}"#.into()],
    )
    .await;
    match client.chat(&request()).await {
        Err(Error::Status { status: 429, message }) => assert_eq!(message, "rate limited"),
        other => panic!("{:?}", other.err()),
    }

    let (client, _rx) = serve(SSE, vec![sse(r#"{"error":{"message":"overloaded"}}"#)]).await;
    let mut stream = client.chat(&request()).await.unwrap();
    assert!(matches!(stream.next().await, Some(Err(Error::Api(m))) if m == "overloaded"));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn dropping_the_stream_closes_the_connection() {
    let mut parts = vec![sse(r#"{"choices":[{"delta":{"content":"one"}}]}"#)];
    // Keep the connection busy long enough that only a drop can end it.
    parts.extend((0..2000).map(|_| ": ping\n\n".to_string()));
    let (client, rx) = serve(SSE, parts).await;
    let mut stream = client.chat(&request()).await.unwrap();
    assert_eq!(stream.next().await.unwrap().unwrap(), Event::Text("one".into()));
    drop(stream);
    // The server's writes start failing well before it would finish (2000 × 5 ms).
    let (_, mut sock) = tokio::time::timeout(Duration::from_secs(3), rx).await.unwrap().unwrap();
    let mut b = [0u8; 1];
    assert_eq!(sock.read(&mut b).await.unwrap_or(0), 0);
}
