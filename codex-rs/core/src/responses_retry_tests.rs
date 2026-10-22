use super::ResponsesStreamRequest;
use super::ResponsesStreamRetryState;
use super::handle_response_stream_error;
use super::log_retry;
use crate::realtime_history::RealtimeHistoryState;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context;
use codex_async_utils::OrCancelExt;
use codex_http_client::RetryAfter;
use codex_protocol::error::CodexErr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing_test::internal::MockWriter;

#[test_case::test_case(ResponsesStreamRequest::Sampling; "sampling")]
#[test_case::test_case(ResponsesStreamRequest::RemoteCompactionV2; "compaction")]
#[tokio::test]
async fn model_capacity_retries_beyond_budget_without_changing_history(
    request: ResponsesStreamRequest,
) {
    let (session, turn_context) = make_session_and_context().await;
    let step_context = StepContext::for_test(Arc::new(turn_context));
    let mut client_session = session.services.model_client.new_session();
    let mut retry_state = ResponsesStreamRetryState {
        retries: 5,
        ..Default::default()
    };
    let history_before = session
        .clone_history()
        .await
        .raw_items()
        .cloned()
        .collect::<Vec<_>>();
    tokio::time::pause();

    for attempt in 0..8 {
        // Neither missing advice nor an arbitrary server deadline changes the fixed delay.
        let err = if attempt % 2 == 0 {
            CodexErr::ServerOverloaded
        } else {
            CodexErr::ServerOverloaded
                .with_retry_after(RetryAfter::from_delay(Duration::from_secs(60)).unwrap())
        };
        let started = Instant::now();
        handle_response_stream_error(
            &mut retry_state,
            /*max_retries*/ 0,
            err,
            &mut client_session,
            &session,
            &step_context,
            request,
        )
        .await
        .expect("capacity must retry even after the normal budget is exhausted");
        // Tokio rounds timer deadlines up to the next millisecond.
        assert!(
            (Duration::from_secs(3)..=Duration::from_millis(3001))
                .contains(&(Instant::now() - started))
        );
        assert_eq!(retry_state.retries, 5);
    }
    assert_eq!(
        session
            .clone_history()
            .await
            .raw_items()
            .collect::<Vec<_>>(),
        history_before.iter().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn model_capacity_wait_can_be_cancelled() {
    let (session, turn_context) = make_session_and_context().await;
    let step_context = StepContext::for_test(Arc::new(turn_context));
    let mut client_session = session.services.model_client.new_session();
    let mut retry_state = ResponsesStreamRetryState::default();
    let cancellation = tokio_util::sync::CancellationToken::new();
    tokio::time::pause();
    let retry = handle_response_stream_error(
        &mut retry_state,
        0,
        CodexErr::ServerOverloaded,
        &mut client_session,
        &session,
        &step_context,
        ResponsesStreamRequest::Sampling,
    )
    .or_cancel(&cancellation);
    tokio::pin!(retry);
    assert!(futures::poll!(&mut retry).is_pending());
    let started = Instant::now();
    cancellation.cancel();
    assert!(retry.await.is_err());
    assert_eq!(Instant::now(), started);
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
