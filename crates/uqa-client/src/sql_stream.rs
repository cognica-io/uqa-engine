//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{HttpEngineError, SQLStreamFrame};

const MAX_STREAM_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Incremental reader for one authenticated UQA NDJSON SQL response.
pub struct SQLStream {
    response: reqwest::Response,
    request_id: String,
    buffer: Vec<u8>,
    scan_start: usize,
    phase: StreamPhase,
    body_finished: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum StreamPhase {
    AwaitingMetadata,
    Rows,
    Terminal,
    Finished,
}

impl SQLStream {
    pub(crate) fn new(response: reqwest::Response, request_id: String) -> Self {
        Self {
            response,
            request_id,
            buffer: Vec::new(),
            scan_start: 0,
            phase: StreamPhase::AwaitingMetadata,
            body_finished: false,
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub async fn next_frame(&mut self) -> Result<Option<SQLStreamFrame>, HttpEngineError> {
        loop {
            if self.phase == StreamPhase::Finished {
                return Ok(None);
            }
            if self.phase == StreamPhase::Terminal {
                return self.finish().await;
            }
            if let Some(newline) = self.next_newline() {
                if newline > MAX_STREAM_FRAME_BYTES {
                    return Err(HttpEngineError::StreamFrameTooLarge);
                }
                let line = take_line(&mut self.buffer, newline);
                self.scan_start = 0;
                if line_is_empty(&line) {
                    continue;
                }
                return self.decode_frame(&line).map(Some);
            }
            if self.buffer.len() > MAX_STREAM_FRAME_BYTES {
                return Err(HttpEngineError::StreamFrameTooLarge);
            }
            if self.body_finished {
                if self.buffer.is_empty() {
                    return Err(HttpEngineError::TruncatedStream);
                }
                if self.buffer.len() > MAX_STREAM_FRAME_BYTES {
                    return Err(HttpEngineError::StreamFrameTooLarge);
                }
                let line = std::mem::take(&mut self.buffer);
                return self.decode_frame(&line).map(Some);
            }
            match self
                .response
                .chunk()
                .await
                .map_err(HttpEngineError::transport)?
            {
                Some(chunk) => {
                    self.buffer.extend_from_slice(&chunk);
                }
                None => self.body_finished = true,
            }
        }
    }

    fn decode_frame(&mut self, line: &[u8]) -> Result<SQLStreamFrame, HttpEngineError> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let frame = serde_json::from_slice::<SQLStreamFrame>(line)
            .map_err(HttpEngineError::InvalidResponse)?;
        if frame
            .request_id()
            .is_some_and(|request_id| request_id != self.request_id)
        {
            return Err(HttpEngineError::StreamRequestIdMismatch);
        }
        self.phase = match (&self.phase, &frame) {
            (StreamPhase::AwaitingMetadata, SQLStreamFrame::Metadata { .. })
            | (StreamPhase::Rows, SQLStreamFrame::Row { .. }) => StreamPhase::Rows,
            (StreamPhase::AwaitingMetadata | StreamPhase::Rows, SQLStreamFrame::Error { .. })
            | (StreamPhase::Rows, SQLStreamFrame::Complete { .. }) => StreamPhase::Terminal,
            _ => return Err(HttpEngineError::InvalidStreamSequence),
        };
        Ok(frame)
    }

    async fn finish(&mut self) -> Result<Option<SQLStreamFrame>, HttpEngineError> {
        loop {
            if let Some(newline) = self.next_newline() {
                if newline > MAX_STREAM_FRAME_BYTES {
                    return Err(HttpEngineError::StreamFrameTooLarge);
                }
                let line = take_line(&mut self.buffer, newline);
                self.scan_start = 0;
                if !line_is_empty(&line) {
                    return Err(HttpEngineError::InvalidStreamSequence);
                }
                continue;
            }
            if self.buffer.len() > MAX_STREAM_FRAME_BYTES {
                return Err(HttpEngineError::StreamFrameTooLarge);
            }
            if self.body_finished {
                if !line_is_empty(&self.buffer) {
                    return Err(HttpEngineError::InvalidStreamSequence);
                }
                self.buffer.clear();
                self.phase = StreamPhase::Finished;
                return Ok(None);
            }
            match self
                .response
                .chunk()
                .await
                .map_err(HttpEngineError::transport)?
            {
                Some(chunk) => {
                    self.buffer.extend_from_slice(&chunk);
                }
                None => self.body_finished = true,
            }
        }
    }

    fn next_newline(&mut self) -> Option<usize> {
        let start = self.scan_start.min(self.buffer.len());
        let newline = self.buffer[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| start + offset);
        if newline.is_none() {
            self.scan_start = self.buffer.len();
        }
        newline
    }
}

fn line_is_empty(line: &[u8]) -> bool {
    line.is_empty() || line == b"\r"
}

fn take_line(buffer: &mut Vec<u8>, newline: usize) -> Vec<u8> {
    let remainder = buffer.split_off(newline + 1);
    let mut line = std::mem::replace(buffer, remainder);
    line.truncate(newline);
    line
}
