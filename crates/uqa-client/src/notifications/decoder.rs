//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict identities, readiness and contiguous sequence admission over bounded SSE framing.

mod envelope;

use super::{
    framing::{self, Framer},
    NotificationReady, NotificationWireEvent, ProtocolError, SubscriptionRequest, TimerLimits,
};
use std::sync::Arc;
use uqa_core::notifications::NotificationRequestId;

/// At most one observation is returned. The caller retains and resubmits the unconsumed suffix of its HTTP chunk; no chunk-sized copy or event queue is hidden in the decoder.
#[derive(Debug)]
pub struct DecodeStep {
    pub consumed: usize,
    pub event: Option<NotificationWireEvent>,
}

/// A single response epoch. This validates the wire contract; HTTP security, actual server readiness, clocks and reconnection belong to the transport adapter.
pub struct NotificationDecoder {
    request: Arc<SubscriptionRequest>,
    expected_request_id: NotificationRequestId,
    timer_limits: TimerLimits,
    framer: Framer,
    ready: Option<NotificationReady>,
    sequence: u64,
    terminal: bool,
    ended: bool,
    failure: Option<ProtocolError>,
}

impl NotificationDecoder {
    pub fn new(
        request: Arc<SubscriptionRequest>,
        expected_request_id: NotificationRequestId,
        timer_limits: TimerLimits,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            request,
            expected_request_id,
            timer_limits,
            framer: Framer::new()?,
            ready: None,
            sequence: 0,
            terminal: false,
            ended: false,
            failure: None,
        })
    }

    /// Returns zero consumed bytes only for empty input or a completed CR-delimited frame that was waiting to disambiguate its exact byte limit. In the latter case an event is returned, so callers can make progress without discarding input.
    pub fn decode(&mut self, input: &[u8]) -> Result<DecodeStep, ProtocolError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        match self.decode_inner(input) {
            Ok(step) => Ok(step),
            Err(error) => self.fail(error),
        }
    }

    fn decode_inner(&mut self, input: &[u8]) -> Result<DecodeStep, ProtocolError> {
        if self.ended {
            return Err(ProtocolError::UnexpectedEnd);
        }
        if self.terminal {
            let consumed = self.framer.consume_terminal_lf(input);
            return if consumed == input.len() {
                Ok(DecodeStep {
                    consumed,
                    event: None,
                })
            } else {
                Err(ProtocolError::EventOrder)
            };
        }
        let step = self.framer.next(input)?;
        let event = if step.complete {
            Some(self.admit_frame()?)
        } else {
            None
        };
        Ok(DecodeStep {
            consumed: step.consumed,
            event,
        })
    }

    /// Call at EOF, then again if this returns an event. EOF can finalize a bare CR at the exact frame limit, but never dispatches an unterminated event. A live stream without a terminal frame ends with `UnexpectedEnd`.
    pub fn finish(&mut self) -> Result<Option<NotificationWireEvent>, ProtocolError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        self.ended = true;
        let result = if self.framer.complete_at_end() {
            self.admit_frame().map(Some)
        } else if self.terminal && self.framer.frame().is_empty() {
            Ok(None)
        } else {
            Err(self.framer.end_error())
        };
        result.or_else(|error| self.fail(error))
    }

    pub fn ready(&self) -> Option<&NotificationReady> {
        self.ready.as_ref()
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal || self.failure.is_some()
    }

    fn fail<T>(&mut self, error: ProtocolError) -> Result<T, ProtocolError> {
        self.failure = Some(error);
        self.framer.clear();
        Err(error)
    }

    fn admit_frame(&mut self) -> Result<NotificationWireEvent, ProtocolError> {
        let event = match framing::fields(self.framer.frame())? {
            Some(fields) => envelope::admit(
                &fields,
                &self.request,
                &self.expected_request_id,
                self.timer_limits,
                self.ready.as_ref(),
                self.sequence,
            )?,
            None => NotificationWireEvent::Heartbeat,
        };
        match &event {
            NotificationWireEvent::Ready(ready) => self.ready = Some(ready.clone()),
            NotificationWireEvent::Notification(
                uqa_core::notifications::NotificationEvent::Notification { sequence, .. },
            ) => self.sequence = *sequence,
            NotificationWireEvent::Error(_) | NotificationWireEvent::ServerDraining { .. } => {
                self.terminal = true;
            }
            _ => {}
        }
        self.framer.clear();
        Ok(event)
    }
}

#[cfg(test)]
mod tests;
