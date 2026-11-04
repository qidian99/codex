use super::ResponsesStreamRequest;
use super::ResponsesStreamRetryState;
use super::handle_response_stream_error;
use super::log_retry;
use crate::realtime_history::RealtimeHistoryState;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context;
use codex_features::Feature;
use codex_http_client::RetryAfter;
use codex_protocol::error::CodexErr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing_test::internal::MockWriter;

#[tokio::test]
async fn capacity_retries_outlive_transport_budget_with_capped_backoff() {
    let (session, mut turn_context) = make_session_and_context().await;
    Arc::make_mut(&mut turn_context.config)
        .features
        .enable(Feature::AutoContinueOnRateLimit);
    let step_context = StepContext::for_test(Arc::new(turn_context));
    let mut client_session = session.services.model_client.new_session();
    let mut retry_state = ResponsesStreamRetryState::default();
    tokio::time::pause();
    for attempt in 1..=8 {
        let start = Instant::now();
        handle_response_stream_error(
            &mut retry_state,
            /*max_retries*/ 0,
            CodexErr::ServerOverloaded,
            &mut client_session,
            &session,
            &step_context,
            ResponsesStreamRequest::Sampling,
        )
        .await
        .expect("capacity retry should not exhaust the transport budget");
        let elapsed = start.elapsed();
        assert!((Duration::from_millis(4500)..=Duration::from_millis(60001)).contains(&elapsed));
        if attempt >= 5 {
            assert!(elapsed >= Duration::from_secs(60));
        }
        assert_eq!(retry_state.capacity_retries, attempt);
        assert_eq!(retry_state.retries, 0);
    }
}

#[tokio::test]
async fn capacity_retries_respect_server_deadline_and_compaction_requests() {
    let (session, mut turn_context) = make_session_and_context().await;
    Arc::make_mut(&mut turn_context.config)
        .features
        .enable(Feature::AutoContinueOnRateLimit);
    let step_context = StepContext::for_test(Arc::new(turn_context));
    let mut client_session = session.services.model_client.new_session();
    let mut retry_state = ResponsesStreamRetryState::default();
    tokio::time::pause();
    let start = Instant::now();
    let advice = RetryAfter::from_delay(Duration::from_secs(90)).expect("retry deadline");
    handle_response_stream_error(
        &mut retry_state,
        /*max_retries*/ 0,
        CodexErr::ServerOverloaded.with_retry_after(advice),
        &mut client_session,
        &session,
        &step_context,
        ResponsesStreamRequest::RemoteCompactionV2,
    )
    .await
    .expect("compaction should wait for capacity too");
    assert!((Duration::from_secs(90)..=Duration::from_millis(90001)).contains(&start.elapsed()));
}

#[tokio::test]
async fn capacity_recovery_does_not_retry_quota_errors_or_disabled_policy() {
    let (session, turn_context) = make_session_and_context().await;
    let mut client_session = session.services.model_client.new_session();
    let step_context = StepContext::for_test(Arc::new(turn_context));
    let mut retry_state = ResponsesStreamRetryState::default();
    assert!(
        handle_response_stream_error(
            &mut retry_state,
            0,
            CodexErr::ServerOverloaded,
            &mut client_session,
            &session,
            &step_context,
            ResponsesStreamRequest::Sampling,
        )
        .await
        .is_err()
    );
    let (session, mut turn_context) = make_session_and_context().await;
    let mut client_session = session.services.model_client.new_session();
    Arc::make_mut(&mut turn_context.config)
        .features
        .enable(Feature::AutoContinueOnRateLimit);
    let step_context = StepContext::for_test(Arc::new(turn_context));
    assert!(
        handle_response_stream_error(
            &mut retry_state,
            0,
            CodexErr::QuotaExceeded,
            &mut client_session,
            &session,
            &step_context,
            ResponsesStreamRequest::Sampling,
        )
        .await
        .is_err()
    );
    assert_eq!(retry_state.capacity_retries, 0);
}

#[tokio::test]
async fn sampling_retry_logs_stream_error_context() {
    let (_session, turn_context) = make_session_and_context().await;
    let buffer: &'static std::sync::Mutex<Vec<u8>> =
        Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(MockWriter::new(buffer))
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    log_retry(
        ResponsesStreamRequest::Sampling,
        &turn_context,
        &CodexErr::Stream("websocket closed by server before response.completed".to_string()),
        /*retries*/ 2,
        /*max_retries*/ 5,
        Duration::from_secs(1),
    );

    let logs = String::from_utf8(
        buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("retry log should be valid utf-8");
    assert!(logs.contains("stream disconnected - retrying sampling request"));
    assert!(logs.contains(&format!("turn_id={}", turn_context.sub_id)));
    assert!(logs.contains("retries=2"));
    assert!(logs.contains("max_retries=5"));
    assert!(logs.contains(
        "sampling_error=stream disconnected before completion: websocket closed by server before response.completed"
    ));
}

/// Time spent reporting a retry must count toward the server's original deadline.
#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "test holds the event-delivery lock to delay notification while virtual time advances"
)]
async fn stream_retry_preserves_deadline_across_delayed_notification() {
    let (mut session, turn_context) = make_session_and_context().await;
    let step_context = StepContext::for_test(Arc::new(turn_context));
    session.realtime_history = Some(Mutex::new(RealtimeHistoryState::default()));
    let mut client_session = session.services.model_client.new_session();
    // The second retry emits a notification even when release builds hide the first.
    let mut retry_state = ResponsesStreamRetryState {
        retries: 1,
        ..Default::default()
    };

    tokio::time::pause();
    let advice = RetryAfter::from_delay(Duration::from_secs(10)).expect("retry deadline");
    tokio::time::advance(Duration::from_secs(4)).await;

    // Hold the event-delivery lock so reporting the error consumes part of the deadline.
    let history_guard = session.realtime_history.as_ref().unwrap().lock().await;
    let retry = handle_response_stream_error(
        &mut retry_state,
        /*max_retries*/ 2,
        CodexErr::InternalServerError.with_retry_after(advice),
        &mut client_session,
        &session,
        &step_context,
        ResponsesStreamRequest::Sampling,
    );
    tokio::pin!(retry);
    assert!(futures::poll!(&mut retry).is_pending());
    tokio::time::advance(Duration::from_secs(4)).await;
    drop(history_guard);

    assert!(futures::poll!(&mut retry).is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(futures::poll!(&mut retry).is_pending());
    retry.await.expect("retry should be allowed");
    // Tokio rounds timer deadlines up to the next millisecond.
    let resumed_at = Instant::now();
    assert!(
        (advice.deadline()..=advice.deadline() + Duration::from_millis(1)).contains(&resumed_at),
        "retry resumed at {resumed_at:?}, expected {advice:?}"
    );
}
