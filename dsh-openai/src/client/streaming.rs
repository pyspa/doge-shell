//! The streaming half of `send_chat_streaming`: one attempt over SSE (`send_once_streaming`/`consume_event_stream`), sharing the same retry
//! and recovery machinery as the non-streaming path in `client.rs`.
use super::*;

impl ChatGptClient {
    pub(super) async fn send_streaming_with_retry(
        &self,
        mut body: Value,
        cancel_check: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        let mut attempt = 0usize;
        let mut recovery = RecoveryState::seed(self.known_unsupported());

        loop {
            match self
                .send_once_streaming(&body, cancel_check, on_delta)
                .await
            {
                Ok(data) => return Ok(data),
                // Some of the reply is already on the screen: retrying would
                // show its start twice, so this is the end of the line.
                Err(StreamError::AfterFirstDelta(err)) => return Err(err),
                Err(StreamError::BeforeFirstDelta(err)) => {
                    if is_ctrl_c_cancelled(&err) {
                        return Err(err);
                    }

                    // Dropping `stream` itself (one of the recoverable fields)
                    // degrades this turn to a single non-streaming response
                    // rather than failing it.
                    if self.recover(&err, &mut body, &mut recovery) {
                        continue;
                    }

                    attempt += 1;
                    let Some(delay) = retry_delay(&err, attempt) else {
                        return Err(err);
                    };

                    debug!(
                        chat_direction = "retry",
                        attempt = attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %err
                    );
                    sleep_with_cancel(delay, cancel_check).await?;
                }
            }
        }
    }

    async fn send_once_streaming(
        &self,
        body: &Value,
        cancel_check: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value, StreamError> {
        let builder = self
            .client
            .post(&self.chat_endpoint)
            // The client's default timeout is a non-streaming turn's budget;
            // a stream can legitimately run longer. `consume_event_stream`
            // enforces its own no-data timeout instead.
            .timeout(Duration::from_secs(config::MAX_TIMEOUT_SECS))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(body);

        let response = Self::await_with_cancel(builder.send(), cancel_check)
            .await
            .map_err(StreamError::BeforeFirstDelta)?;

        let status = response.status();
        let retry_after = parse_retry_after(response.headers());
        let is_event_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));

        if !status.is_success() {
            let text = Self::await_with_cancel(response.text(), cancel_check)
                .await
                .map_err(StreamError::BeforeFirstDelta)?;
            return Err(StreamError::BeforeFirstDelta(
                ApiError {
                    status: Some(status.as_u16()),
                    retry_after,
                    message: error_message_from_body(&text, status),
                }
                .into(),
            ));
        }

        if !is_event_stream {
            // The endpoint accepted the request but ignored `stream: true`
            // and answered in one JSON object, as some OpenAI-compatible
            // servers do. Read it the same way `send_once` would - but under
            // the operator's budget, not the streaming override above: this
            // degraded reply is an ordinary non-streaming turn, and leaving
            // it under `MAX_TIMEOUT_SECS` silently disabled
            // `AI_CHAT_TIMEOUT_SECS` for every such endpoint.
            let text = tokio::time::timeout(
                self.request_timeout,
                Self::await_with_cancel(response.text(), cancel_check),
            )
            .await
            .map_err(|_| {
                StreamError::BeforeFirstDelta(anyhow!(
                    "the OpenAI request timed out after {:?}",
                    self.request_timeout
                ))
            })?
            .map_err(StreamError::BeforeFirstDelta)?;
            let data: Value = serde_json::from_str(&text).map_err(|err| {
                StreamError::BeforeFirstDelta(anyhow!("failed to parse the OpenAI response: {err}"))
            })?;
            if let Some(message) = error_message_from_value(&data) {
                return Err(StreamError::BeforeFirstDelta(
                    ApiError {
                        status: None,
                        retry_after,
                        message,
                    }
                    .into(),
                ));
            }
            usage::record_response(&data);
            return Ok(data);
        }

        self.consume_event_stream(response, cancel_check, on_delta)
            .await
    }

    /// Read one SSE response to completion, aggregating it into the same
    /// shape `send_once` would return.
    async fn consume_event_stream(
        &self,
        response: reqwest::Response,
        cancel_check: Option<&dyn Fn() -> bool>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Value, StreamError> {
        use futures::StreamExt;

        let mut byte_stream = response.bytes_stream();
        let mut splitter = SseFrameSplitter::new();
        let mut aggregator = DeltaAggregator::new();
        let mut delta_sent = false;

        // Held for the whole stream, not recreated per chunk: a signal
        // handler is only meaningful to register once.
        let ctrl_c_future = async {
            if cancel_check.is_some() {
                std::future::pending::<bool>().await
            } else {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => true,
                    Err(e) => {
                        debug!("dsh-openai: Failed to listen for Ctrl+C via tokio: {}", e);
                        std::future::pending::<bool>().await
                    }
                }
            }
        };
        tokio::pin!(ctrl_c_future);

        let mut poll_interval = tokio::time::interval(Duration::from_millis(50));
        poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            // Freshly armed each iteration: the budget is silence *between*
            // chunks, not a cap on the whole reply.
            let idle_deadline = tokio::time::sleep(self.request_timeout);
            tokio::pin!(idle_deadline);
            let next_item = byte_stream.next();
            tokio::pin!(next_item);

            let poll_result = loop {
                tokio::select! {
                    item = &mut next_item => break Ok(item),
                    true = &mut ctrl_c_future => break Err(RequestCancelled.into()),
                    _ = &mut idle_deadline => break Err(anyhow!(
                        "no data received from the OpenAI stream for {:?}",
                        self.request_timeout
                    )),
                    _ = poll_interval.tick() => {
                        if let Some(check) = cancel_check
                            && check() {
                                break Err(RequestCancelled.into());
                            }
                    }
                }
            };

            let item = match poll_result {
                Ok(item) => item,
                Err(err) => return Err(Self::wrap_stream_error(err, delta_sent)),
            };

            let chunk = match item {
                Some(Ok(chunk)) => chunk,
                Some(Err(err)) => return Err(Self::wrap_stream_error(err.into(), delta_sent)),
                None => {
                    // The connection closed without a trailing blank line
                    // after the last event - flush whatever is pending.
                    for payload in splitter.finish() {
                        Self::apply_stream_payload(
                            &payload,
                            &mut aggregator,
                            &mut delta_sent,
                            on_delta,
                        )?;
                    }
                    // No `[DONE]` and no `finish_reason` means the
                    // connection dropped mid-turn, not that the model
                    // finished. Without this check the partial content
                    // collected so far would be returned as a normal,
                    // complete `Answer` - a truncated reply with no sign
                    // anything went wrong.
                    if !aggregator.has_finish_reason() {
                        return Err(Self::wrap_stream_error(
                            anyhow!(
                                "the OpenAI stream closed before the reply finished \
                                 (no finish_reason received)"
                            ),
                            delta_sent,
                        ));
                    }
                    let value = aggregator.finish();
                    usage::record_response(&value);
                    return Ok(value);
                }
            };

            for payload in splitter.push(&chunk) {
                if is_done_marker(&payload) {
                    let value = aggregator.finish();
                    usage::record_response(&value);
                    return Ok(value);
                }
                Self::apply_stream_payload(&payload, &mut aggregator, &mut delta_sent, on_delta)?;
            }
        }
    }

    fn wrap_stream_error(err: Error, delta_sent: bool) -> StreamError {
        if delta_sent {
            StreamError::AfterFirstDelta(err)
        } else {
            StreamError::BeforeFirstDelta(err)
        }
    }

    /// Decode one SSE frame payload and, if it carried a text delta, forward
    /// it to the caller.
    ///
    /// A single malformed frame is not fatal to the turn - it is dropped
    /// with a debug log, the way a client tolerates one corrupted keep-alive
    /// rather than losing an otherwise complete answer.
    pub(super) fn apply_stream_payload(
        payload: &str,
        aggregator: &mut DeltaAggregator,
        delta_sent: &mut bool,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<(), StreamError> {
        let chunk: Value = match serde_json::from_str(payload) {
            Ok(chunk) => chunk,
            Err(err) => {
                debug!(
                    chat_direction = "response",
                    reason = "malformed SSE frame",
                    error = %err
                );
                return Ok(());
            }
        };

        // A provider can report a mid-stream failure as its own frame
        // (`data: {"error": {...}}`) instead of a non-2xx status - the same
        // shape `error_message_from_value` already handles for a plain JSON
        // response. This frame has no `choices` array, so without this
        // check `DeltaAggregator::apply`'s early return would treat it as
        // an empty heartbeat, and the caller would see a stall or a
        // truncated answer instead of the real failure.
        if let Some(message) = error_message_from_value(&chunk) {
            let err = ApiError {
                status: None,
                retry_after: None,
                message,
            };
            return Err(Self::wrap_stream_error(err.into(), *delta_sent));
        }

        if let Some(text) = aggregator.apply(&chunk) {
            *delta_sent = true;
            on_delta(&text);
        }
        Ok(())
    }

    pub(super) async fn await_with_cancel<F, T, E>(
        future: F,
        cancel_check: Option<&dyn Fn() -> bool>,
    ) -> Result<T>
    where
        F: Future<Output = Result<T, E>>,
        anyhow::Error: From<E>,
    {
        tokio::pin!(future);

        // Attempt to listen for Ctrl+C only if we don't have an external check
        // If an external check is provided, we assume the caller handles signals and updates the check state.
        let ctrl_c_future = async {
            if cancel_check.is_some() {
                std::future::pending::<bool>().await
            } else {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => true,
                    Err(e) => {
                        debug!("dsh-openai: Failed to listen for Ctrl+C via tokio: {}", e);
                        std::future::pending::<bool>().await
                    }
                }
            }
        };
        tokio::pin!(ctrl_c_future);

        // Check for cancellation more frequently for better responsiveness
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        // Ensure the first tick completes immediately so we don't wait 50ms unnecessarily
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                res = &mut future => return res.map_err(anyhow::Error::from),
                // If tokio's ctrl_c fires, treat it as a cancellation
                true = &mut ctrl_c_future => return Err(RequestCancelled.into()),
                _ = interval.tick() => {
                    if let Some(check) = cancel_check
                        && check() {
                            return Err(RequestCancelled.into());
                        }
                }
            }
        }
    }
}
