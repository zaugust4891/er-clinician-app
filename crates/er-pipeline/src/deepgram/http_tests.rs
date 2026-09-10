//! Exercise the production request/retry/response path with synthetic data.
//! The endpoint override and plain HTTP client exist only in this test build.

use super::*;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

const AUDIO: &[u8] = b"synthetic-audio-fixture";
const KEY: &str = "synthetic-api-key";

#[derive(Clone)]
struct Request {
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Reply {
    headers: String,
    body: Vec<u8>,
    header_delay: Duration,
    body_delay: Duration,
}

impl Reply {
    fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        let body = body.into();
        Self {
            headers: format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ),
            body,
            header_delay: Duration::ZERO,
            body_delay: Duration::ZERO,
        }
    }

    fn success() -> Self {
        Self::new(
            200,
            serde_json::json!({
                "metadata": { "duration": 1.25 },
                "results": { "channels": [{ "alternatives": [{
                    "transcript": "Synthetic response.", "confidence": 0.95
                }] }] }
            })
            .to_string(),
        )
    }
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/listen", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            let mut replies: VecDeque<_> = replies.into();
            // Dropping the fixture cancels pending response tasks as well.
            let mut connections = JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let recorded = recorded.clone();
                let reply = replies
                    .pop_front()
                    .unwrap_or_else(|| Reply::new(500, "unexpected request"));
                connections.spawn(async move {
                    let request = read_request(&mut socket).await;
                    recorded.lock().unwrap().push(request);
                    tokio::time::sleep(reply.header_delay).await;
                    if socket.write_all(reply.headers.as_bytes()).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(reply.body_delay).await;
                    let _ = socket.write_all(&reply.body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }

    fn transcriber(&self, timeout: Duration) -> DeepgramTranscriber {
        let mut stt = DeepgramTranscriber::new(KEY).unwrap();
        stt.http = http_client_builder()
            .https_only(false)
            .no_proxy()
            .timeout(timeout)
            .build()
            .unwrap();
        stt.listen_url = self.url.clone();
        stt
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(socket: &mut TcpStream) -> Request {
    let mut raw = Vec::new();
    let header_end = loop {
        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(raw.len() < 16 * 1024);
        assert!(socket.read_buf(&mut raw).await.unwrap() > 0);
    };
    let head = String::from_utf8(raw[..header_end].to_vec()).unwrap();
    let mut lines = head.lines();
    let first = lines.next().unwrap();
    assert!(first.starts_with("POST "));
    let target = first.split_whitespace().nth(1).unwrap().to_owned();
    let headers: BTreeMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers["content-length"].parse().unwrap();
    assert!(length < 16 * 1024);
    while raw.len() < header_end + length {
        assert!(socket.read_buf(&mut raw).await.unwrap() > 0);
    }
    Request {
        target,
        headers,
        body: raw[header_end..header_end + length].to_vec(),
    }
}

#[tokio::test]
async fn request_contract_and_dispatch_preserve_audio_and_privacy_flags() {
    let server = Server::start(vec![Reply::success()]).await;
    let stt = crate::speech_to_text::AppSpeechToText::Deepgram(server.transcriber(ATTEMPT_TIMEOUT));
    let result = stt.transcribe(AUDIO, "audio/wav").await.unwrap();
    assert_eq!(result.text, "Synthetic response.");
    assert_eq!(result.audio_duration_secs, 1.25);
    assert_eq!(result.confidence, Some(0.95));
    assert!(result.processing_duration_secs > 0.0);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    let url = reqwest::Url::parse(&format!("http://localhost{}", request.target)).unwrap();
    assert_eq!(url.path(), "/v1/listen");
    assert_eq!(
        url.query_pairs().collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("model".into(), "nova-3-medical".into()),
            ("smart_format".into(), "true".into()),
            ("diarize".into(), "true".into()),
            ("utterances".into(), "true".into()),
            ("mip_opt_out".into(), "true".into()),
        ])
    );
    assert_eq!(request.headers["authorization"], format!("Token {KEY}"));
    assert_eq!(request.headers["content-type"], "audio/wav");
    assert_eq!(request.body, AUDIO);
    assert!(!request.target.contains(KEY));
}

#[tokio::test]
async fn transient_statuses_retry_then_succeed_with_identical_requests() {
    let server = Server::start(vec![
        Reply::new(429, ""),
        Reply::new(503, ""),
        Reply::success(),
    ])
    .await;
    let result = server
        .transcriber(ATTEMPT_TIMEOUT)
        .transcribe(AUDIO, "audio/ogg")
        .await
        .unwrap();
    assert!(result.processing_duration_secs >= 1.5);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request.target, requests[0].target);
        assert_eq!(request.headers["authorization"], format!("Token {KEY}"));
        assert_eq!(request.body, AUDIO);
    }
}

#[tokio::test]
async fn retries_are_bounded_and_vendor_errors_never_include_content() {
    let server = Server::start(
        (0..4)
            .map(|_| Reply::new(503, "synthetic-patient-details synthetic-api-key"))
            .collect(),
    )
    .await;
    let error = server
        .transcriber(ATTEMPT_TIMEOUT)
        .transcribe(AUDIO, "audio/wav")
        .await
        .unwrap_err();
    assert_eq!(server.requests().len(), MAX_ATTEMPTS as usize);
    assert_eq!(error.to_string(), "transcription error: deepgram HTTP 503");
    assert!(!format!("{error:?}").contains("synthetic-"));
}

#[tokio::test]
async fn permanent_statuses_fail_once_without_reading_the_error_body() {
    for status in [400, 401, 403, 413] {
        let mut reply = Reply::new(status, "synthetic-patient-details synthetic-api-key");
        reply.body_delay = Duration::from_secs(5);
        let server = Server::start(vec![reply]).await;
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            server
                .transcriber(ATTEMPT_TIMEOUT)
                .transcribe(AUDIO, "audio/wav"),
        )
        .await
        .expect("error body must not be read")
        .unwrap_err();
        assert_eq!(server.requests().len(), 1);
        assert_eq!(
            error.to_string(),
            format!("transcription error: deepgram HTTP {status}")
        );
    }
}

#[tokio::test]
async fn redirects_cannot_replay_audio_or_credentials() {
    for status in [301, 302, 303, 307, 308] {
        let sink = Server::start(vec![Reply::success()]).await;
        let mut redirect = Reply::new(status, "");
        redirect.headers = format!(
            "HTTP/1.1 {status} Redirect\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            sink.url
        );
        let server = Server::start(vec![redirect]).await;
        let error = server
            .transcriber(ATTEMPT_TIMEOUT)
            .transcribe(AUDIO, "audio/wav")
            .await
            .unwrap_err();
        assert_eq!(server.requests().len(), 1);
        assert!(sink.requests().is_empty());
        assert_eq!(
            error.to_string(),
            format!("transcription error: deepgram HTTP {status}")
        );
    }
}

#[tokio::test]
async fn production_client_refuses_plain_http() {
    let server = Server::start(vec![]).await;
    let mut stt = DeepgramTranscriber::new(KEY).unwrap();
    stt.listen_url = server.url.clone();
    let error = stt.transcribe(AUDIO, "audio/wav").await.unwrap_err();
    assert!(server.requests().is_empty());
    assert!(
        error
            .to_string()
            .contains("invalid Deepgram request configuration")
    );
}

#[tokio::test]
async fn invalid_success_bodies_fail_once_without_echoing_content() {
    for body in [
        "synthetic-patient-details",
        r#"{"results":{}}"#,
        r#"{"synthetic-patient-details":NaN}"#,
    ] {
        let server = Server::start(vec![Reply::new(200, body)]).await;
        let error = server
            .transcriber(ATTEMPT_TIMEOUT)
            .transcribe(AUDIO, "audio/wav")
            .await
            .unwrap_err();
        assert_eq!(server.requests().len(), 1);
        assert!(!format!("{error:?}").contains("synthetic-patient-details"));
    }
}

#[tokio::test]
async fn response_limits_cover_declared_and_chunked_bodies() {
    let mut declared = Reply::new(200, "");
    declared.headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_RESPONSE_BYTES + 1
    );
    declared.body_delay = Duration::from_secs(5);
    let oversized = vec![b' '; MAX_RESPONSE_BYTES + 1];
    let mut chunked = Reply::new(200, "");
    chunked.headers =
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".into();
    chunked.body = format!("{:x}\r\n", oversized.len()).into_bytes();
    chunked.body.extend_from_slice(&oversized);
    chunked.body.extend_from_slice(b"\r\n0\r\n\r\n");
    for reply in [declared, chunked] {
        let server = Server::start(vec![reply]).await;
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            server
                .transcriber(ATTEMPT_TIMEOUT)
                .transcribe(AUDIO, "audio/wav"),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(server.requests().len(), 1);
        assert!(error.to_string().contains("response exceeds size limit"));
    }
}

#[tokio::test]
async fn timeouts_before_headers_and_during_body_are_not_retried() {
    for stall_body in [false, true] {
        let mut reply = Reply::success();
        if stall_body {
            reply.body_delay = Duration::from_secs(5);
        } else {
            reply.header_delay = Duration::from_secs(5);
        }
        let server = Server::start(vec![reply]).await;
        let error = server
            .transcriber(Duration::from_millis(100))
            .transcribe(AUDIO, "audio/wav")
            .await
            .unwrap_err();
        assert_eq!(server.requests().len(), 1);
        assert!(error.to_string().contains("request timed out"));
    }
}

#[tokio::test]
async fn broken_connections_before_headers_and_during_body_can_recover() {
    for during_body in [false, true] {
        let mut reply = Reply::new(200, "");
        reply.headers = if during_body {
            "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n".into()
        } else {
            String::new()
        };
        let server = Server::start(vec![reply, Reply::success()]).await;
        let result = server
            .transcriber(ATTEMPT_TIMEOUT)
            .transcribe(AUDIO, "audio/wav")
            .await
            .unwrap();
        assert_eq!(server.requests().len(), 2);
        assert_eq!(result.text, "Synthetic response.");
    }
}
