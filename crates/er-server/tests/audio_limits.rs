//! S1 wire tests: missing Content-Length and a stalled S3 response cannot evade limits.
use er_server::{audio, aws, config::WorkerLimits};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn unknown_length_s3_download_stops_at_the_byte_cap() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n0\r\n\r\n").await.unwrap();
    });
    let clients = aws::lazy_test_clients(&endpoint, "http://127.0.0.1:1");
    let limits = WorkerLimits {
        max_audio_bytes: 4,
        download_timeout: Duration::from_secs(2),
        ..Default::default()
    };
    let error = audio::download(&clients.s3, "synthetic", "synthetic", "audio/wav", &limits)
        .await
        .unwrap_err();
    assert_eq!(error.0, "chunk_too_large");
    server.await.unwrap();
}

#[tokio::test]
async fn download_deadline_includes_waiting_for_s3_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let clients = aws::lazy_test_clients(&endpoint, "http://127.0.0.1:1");
    let limits = WorkerLimits {
        download_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        audio::download(&clients.s3, "synthetic", "synthetic", "audio/wav", &limits),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.0, "audio_download_timeout");
    server.abort();
}
