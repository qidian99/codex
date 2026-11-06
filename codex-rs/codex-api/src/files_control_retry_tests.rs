use super::tests::base_url_for;
use super::tests::chatgpt_auth;
use super::tests::default_http_client_pool;
use super::*;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn creation_and_confirmation_recover_without_repeating_completed_stages() {
    let server = MockServer::start().await;
    let creates = AtomicUsize::new(0);
    let upload_url = format!("{}/blob", server.uri());
    Mock::given(method("POST"))
        .and(path("/backend-api/files"))
        .respond_with(move |_: &Request| {
            if creates.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503).insert_header("Retry-After", "0")
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "file_id": "file_recovered", "upload_url": upload_url,
                }))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/blob"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let confirmations = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/backend-api/files/file_recovered/uploaded"))
        .respond_with(move |_: &Request| {
            match confirmations.fetch_add(1, Ordering::SeqCst) {
                // An outage longer than the processing-poll budget must not exhaust it.
                0 => ResponseTemplate::new(429).insert_header("Retry-After", "31"),
                1 => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "retry"}))
                }
                _ => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "status": "success", "download_url": "https://example.com/recovered",
                })),
            }
        })
        .expect(3)
        .mount(&server)
        .await;
    let mut opens = 0;
    let uploaded = upload_openai_file(
        &base_url_for(&server),
        &chatgpt_auth(),
        &default_http_client_pool(),
        "hello.txt".into(),
        5,
        || {
            opens += 1;
            async { Ok(futures::stream::iter([Ok(Bytes::from_static(b"hello"))])) }
        },
        None,
    )
    .await
    .expect("upload should recover at each control stage");
    assert_eq!(uploaded.file_id, "file_recovered");
    assert_eq!(
        opens, 1,
        "confirmation retry must not re-upload the contents"
    );
    let requests = server.received_requests().await.unwrap();
    let creates: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/backend-api/files")
        .collect();
    assert_eq!(creates[0].body, creates[1].body);
    assert_eq!(
        creates[0].headers["idempotency-key"],
        creates[1].headers["idempotency-key"]
    );
    assert!(!creates[0].headers["idempotency-key"].is_empty());
    server.verify().await;
}

#[tokio::test]
async fn disconnected_and_truncated_create_responses_retry_the_same_request() {
    for truncate_body in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut received = Vec::new();
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    assert!(stream.read_buf(&mut bytes).await.unwrap() > 0);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                received.push(bytes);
                if attempt == 0 {
                    if truncate_body {
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{").await.unwrap();
                    }
                    // Close after accepting the body: the server may already have created a record.
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .await
                        .unwrap();
                }
            }
            received
        });
        let body = send_file_json_request(
            &default_http_client_pool(),
            &chatgpt_auth(),
            &format!("http://{address}/files"),
            &serde_json::json!({"file_name": "hello.txt"}),
            "create",
            Some("same-operation"),
        )
        .await
        .expect("transport failure should recover");
        assert_eq!(body, "{}");
        let requests = peer.await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert!(
                String::from_utf8_lossy(request)
                    .to_lowercase()
                    .contains("idempotency-key: same-operation\r\n")
            );
        }
        let bodies: Vec<_> = requests
            .iter()
            .map(|request| {
                let end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                &request[end + 4..]
            })
            .collect();
        assert_eq!(bodies[0], bodies[1]);
    }
}

#[tokio::test]
async fn permanent_control_failures_return_without_allocating_or_uploading() {
    for (status, body) in [
        (
            400,
            serde_json::json!({"error": {"code": "invalid_request"}}),
        ),
        (401, serde_json::json!({"error": {"code": "unauthorized"}})),
        (403, serde_json::json!({"error": {"code": "forbidden"}})),
        (
            413,
            serde_json::json!({"error": {"code": "file_too_large"}}),
        ),
        (
            429,
            serde_json::json!({"error": {"type": "insufficient_quota", "code": "insufficient_quota"}}),
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/backend-api/files"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let result = upload_openai_file(
            &base_url_for(&server),
            &chatgpt_auth(),
            &default_http_client_pool(),
            "hello.txt".into(),
            5,
            || async {
                Err::<futures::stream::Empty<std::io::Result<Bytes>>, _>(std::io::Error::other(
                    "unexpected contents read",
                ))
            },
            None,
        )
        .await;
        assert!(
            matches!(result, Err(OpenAiFileError::UnexpectedStatus { status: actual, .. }) if actual.as_u16() == status)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancelling_control_retry_does_not_leave_background_requests() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/files"))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "3600"))
        .expect(1)
        .mount(&server)
        .await;
    let url = format!("{}/files", server.uri());
    let task = tokio::spawn(async move {
        send_file_json_request(
            &default_http_client_pool(),
            &chatgpt_auth(),
            &url,
            &serde_json::json!({}),
            "create",
            Some("cancelled"),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.verify().await;
}

#[test]
fn control_retry_classifies_transient_and_permanent_errors() {
    for status in [408, 429, 500, 502, 503, 504] {
        assert!(retryable_file_request_error(
            &OpenAiFileError::UnexpectedStatus {
                url: "https://example.com/files".into(),
                status: StatusCode::from_u16(status).unwrap(),
                body: String::new(),
            }
        ));
    }
    for source in [
        RouteAwareRequestError::Build("invalid request".into()),
        RouteAwareRequestError::TooManyRedirects,
    ] {
        assert!(!retryable_file_request_error(&OpenAiFileError::Request {
            url: "https://example.com/files".into(),
            source
        }));
    }
    assert!(retryable_file_request_error(&OpenAiFileError::Request {
        url: "https://example.com/files".into(),
        source: RouteAwareRequestError::Timeout
    }));
}
