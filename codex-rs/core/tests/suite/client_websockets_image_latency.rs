use super::*;
use codex_protocol::config_types::Verbosity;
use pretty_assertions::assert_eq;

#[test_case::test_case(None, Some(Verbosity::Low); "default to low")]
#[test_case::test_case(Some(Verbosity::Low), Some(Verbosity::High); "low to high")]
#[test_case::test_case(Some(Verbosity::High), None; "high to default")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verbosity_changes_do_not_reupload_image_history(
    first_verbosity: Option<Verbosity>,
    second_verbosity: Option<Verbosity>,
) {
    skip_if_no_network!();
    let server = start_websocket_server(vec![vec![
        vec![ev_response_created("resp-1"), ev_completed("resp-1")],
        vec![ev_response_created("resp-2"), ev_completed("resp-2")],
    ]])
    .await;
    let harness = websocket_harness_with_v2(&server, /*runtime_metrics_enabled*/ true).await;
    let mut session = harness.client.new_session();
    // The transport passes image bytes through; model-side image decoding is not part of this test.
    let image_message: ResponseItem = serde_json::from_value(json!({
        "type": "message", "role": "user", "content": [{
            "type": "input_image",
            "image_url": format!("data:image/png;base64,{}", "A".repeat(1024 * 1024))
        }]
    }))
    .unwrap();
    let first = prompt_with_input(vec![image_message.clone()]);
    let second = prompt_with_input(vec![image_message, message_item("continue")]);
    let mut model = harness.model_info.clone();
    model.support_verbosity = true;
    model.default_verbosity = first_verbosity;
    stream_until_complete_with_model_info(&mut session, &harness, &first, &model, "resp-1").await;
    model.default_verbosity = second_verbosity;
    stream_until_complete_with_model_info(&mut session, &harness, &second, &model, "resp-2").await;

    let requests = server.single_connection();
    assert_eq!(requests.len(), 2);
    let request = requests[1].body_json();
    assert_eq!(request["previous_response_id"], json!("resp-1"));
    assert_eq!(
        request["input"],
        serde_json::to_value(&second.input[1..]).unwrap()
    );
    assert_eq!(
        request["text"]["verbosity"],
        serde_json::to_value(second_verbosity).unwrap()
    );
    assert!(serde_json::to_vec(&request).unwrap().len() < 4096);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_image_request_uses_http_without_sending_or_retrying_websocket() {
    skip_if_no_network!();
    let ws_server = start_websocket_server(vec![vec![vec![ev_completed("unexpected")]]]).await;
    let http_server = core_test_support::responses::start_mock_server().await;
    let response = core_test_support::responses::mount_sse_once(
        &http_server,
        core_test_support::responses::sse(vec![
            ev_response_created("resp-http"),
            ev_completed("resp-http"),
        ]),
    )
    .await;
    // Route upgrade and POST connections to the existing wire fixtures on a shared endpoint.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let ws_address = ws_server.uri().trim_start_matches("ws://").to_string();
    let http_address = http_server.uri().trim_start_matches("http://").to_string();
    let proxy = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut incoming, _) = listener.accept().await.unwrap();
            let ws_address = ws_address.clone();
            let http_address = http_address.clone();
            connections.spawn(async move {
                let mut first = [0];
                if incoming.peek(&mut first).await.unwrap() == 0 {
                    return;
                }
                let target = if first[0] == b'G' {
                    ws_address
                } else {
                    http_address
                };
                let mut outgoing = tokio::net::TcpStream::connect(target).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
            });
        }
    });
    let mut provider = websocket_provider(&ws_server);
    provider.base_url = Some(format!("http://{address}/v1"));
    let harness = websocket_harness_with_provider_options(
        provider,
        /*runtime_metrics_enabled*/ true,
        /*concurrent_reasoning_summaries_enabled*/ false,
        &[],
    )
    .await;
    let image_message = serde_json::from_value(json!({
        "type": "message", "role": "user", "content": [{
            "type": "input_image",
            "image_url": format!("data:image/png;base64,{}", "A".repeat(16 * 1024 * 1024))
        }]
    }))
    .unwrap();
    let prompt = prompt_with_input(vec![image_message]);
    let mut session = harness.client.new_session();
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 20),
        stream_until_complete_with_model_info(
            &mut session,
            &harness,
            &prompt,
            &harness.model_info,
            "resp-http",
        ),
    )
    .await
    .expect("oversized request must fall back immediately");
    assert_eq!(ws_server.handshakes().len(), 1);
    assert!(ws_server.connections().iter().all(Vec::is_empty));
    assert_eq!(
        response.single_request().body_json()["input"],
        serde_json::to_value(&prompt.input).unwrap()
    );
    proxy.abort();
    ws_server.shutdown().await;
}
