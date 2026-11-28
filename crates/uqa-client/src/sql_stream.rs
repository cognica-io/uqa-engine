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
    lines: FrameLines,
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
            lines: FrameLines::default(),
            phase: StreamPhase::AwaitingMetadata,
            body_finished: false,
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub async fn next_frame(&mut self) -> Result<Option<SQLStreamFrame>, HttpEngineError> {
        if self.phase == StreamPhase::Finished {
            return Ok(None);
        }
        loop {
            if let Some(line) = self.lines.next_line(MAX_STREAM_FRAME_BYTES)? {
                if line_is_empty(line) {
                    continue;
                }
                if self.phase == StreamPhase::Terminal {
                    return Err(HttpEngineError::InvalidStreamSequence);
                }
                return self.phase.decode_frame(line, &self.request_id).map(Some);
            }
            if self.body_finished {
                if self.phase == StreamPhase::Terminal {
                    if !line_is_empty(self.lines.remaining()) {
                        return Err(HttpEngineError::InvalidStreamSequence);
                    }
                    self.lines = FrameLines::default();
                    self.phase = StreamPhase::Finished;
                    return Ok(None);
                }
                let line = self.lines.take_remaining();
                if line.is_empty() {
                    return Err(HttpEngineError::TruncatedStream);
                }
                return self.phase.decode_frame(line, &self.request_id).map(Some);
            }
            match self
                .response
                .chunk()
                .await
                .map_err(HttpEngineError::transport)?
            {
                Some(chunk) => self.lines.append(&chunk),
                None => self.body_finished = true,
            }
        }
    }
}

impl StreamPhase {
    fn decode_frame(
        &mut self,
        line: &[u8],
        request_id: &str,
    ) -> Result<SQLStreamFrame, HttpEngineError> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let mut frame = serde_json::from_slice::<SQLStreamFrame>(line)
            .map_err(HttpEngineError::InvalidResponse)?;
        if let SQLStreamFrame::Error {
            code, diagnostic, ..
        } = &mut frame
        {
            if diagnostic
                .as_ref()
                .is_some_and(|value| !value.matches_code(code))
            {
                *diagnostic = None;
            }
        }
        if frame
            .request_id()
            .is_some_and(|frame_id| frame_id != request_id)
        {
            return Err(HttpEngineError::StreamRequestIdMismatch);
        }
        *self = match (&*self, &frame) {
            (StreamPhase::AwaitingMetadata, SQLStreamFrame::Metadata { .. })
            | (StreamPhase::Rows, SQLStreamFrame::Row { .. }) => StreamPhase::Rows,
            (StreamPhase::AwaitingMetadata | StreamPhase::Rows, SQLStreamFrame::Error { .. })
            | (StreamPhase::Rows, SQLStreamFrame::Complete { .. }) => StreamPhase::Terminal,
            _ => return Err(HttpEngineError::InvalidStreamSequence),
        };
        Ok(frame)
    }
}

#[derive(Default)]
struct FrameLines {
    bytes: Vec<u8>,
    read_offset: usize,
    scan_start: usize,
}

impl FrameLines {
    fn next_line(&mut self, limit: usize) -> Result<Option<&[u8]>, HttpEngineError> {
        let Some(offset) = self.bytes[self.scan_start..]
            .iter()
            .position(|&byte| byte == b'\n')
        else {
            self.scan_start = self.bytes.len();
            if self.remaining().len() > limit {
                return Err(HttpEngineError::StreamFrameTooLarge);
            }
            return Ok(None);
        };
        let newline = self.scan_start + offset;
        if newline - self.read_offset > limit {
            return Err(HttpEngineError::StreamFrameTooLarge);
        }
        let start = self.read_offset;
        self.read_offset = newline + 1;
        self.scan_start = self.read_offset;
        Ok(Some(&self.bytes[start..newline]))
    }

    fn append(&mut self, chunk: &[u8]) {
        let unread = self.remaining().len();
        if self.read_offset > 0 && self.read_offset >= unread {
            // Each moved byte is paid for by a byte already consumed since the previous compaction.
            self.bytes.copy_within(self.read_offset.., 0);
            self.bytes.truncate(unread);
            self.scan_start -= self.read_offset;
            self.read_offset = 0;
        }
        self.bytes.extend_from_slice(chunk);
    }

    fn remaining(&self) -> &[u8] {
        &self.bytes[self.read_offset..]
    }

    fn take_remaining(&mut self) -> &[u8] {
        let start = self.read_offset;
        self.read_offset = self.bytes.len();
        &self.bytes[start..]
    }
}

fn line_is_empty(line: &[u8]) -> bool {
    line.is_empty() || line == b"\r"
}

#[cfg(test)]
mod tests;
