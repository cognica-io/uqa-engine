//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One bounded SSE block, without retaining the caller's HTTP chunk or queuing decoded events.

use super::{ProtocolError, MAX_NOTIFICATION_WIRE_BYTES};
use std::borrow::Cow;

const BOM: &[u8; 3] = b"\xef\xbb\xbf";

#[derive(Clone, Copy, Default)]
enum PendingLF {
    #[default]
    None,
    InFrame,
    AfterFrame,
}

pub(super) struct Framer {
    bytes: Vec<u8>,
    line_start: usize,
    pending_lf: PendingLF,
    bom_prefix: usize,
    at_start: bool,
    deferred: bool,
}

pub(super) struct FrameStep {
    pub consumed: usize,
    pub complete: bool,
}

impl Framer {
    pub fn new() -> Result<Self, ProtocolError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(MAX_NOTIFICATION_WIRE_BYTES)
            .map_err(|_| ProtocolError::Allocation)?;
        Ok(Self {
            bytes,
            line_start: 0,
            pending_lf: PendingLF::None,
            bom_prefix: 0,
            at_start: true,
            deferred: false,
        })
    }

    pub fn next(&mut self, input: &[u8]) -> Result<FrameStep, ProtocolError> {
        let mut consumed = 0;
        loop {
            // A CR at the exact limit cannot be published until it is known whether its optional LF would exceed the complete-frame limit.
            if self.deferred {
                match input.get(consumed) {
                    Some(b'\n') => return Err(ProtocolError::ByteLimit),
                    Some(_) => {
                        self.deferred = false;
                        return Ok(FrameStep {
                            consumed,
                            complete: true,
                        });
                    }
                    None => {
                        return Ok(FrameStep {
                            consumed,
                            complete: false,
                        })
                    }
                }
            }
            let Some(&byte) = input.get(consumed) else {
                return Ok(FrameStep {
                    consumed,
                    complete: false,
                });
            };
            consumed += 1;
            if self.at_start && self.consume_prefix(byte)? {
                continue;
            }
            match std::mem::take(&mut self.pending_lf) {
                PendingLF::InFrame if byte == b'\n' => {
                    self.push(byte)?;
                    self.line_start = self.bytes.len();
                    continue;
                }
                PendingLF::AfterFrame if byte == b'\n' => continue,
                _ => {}
            }
            self.push(byte)?;
            if !matches!(byte, b'\r' | b'\n') {
                continue;
            }
            let empty = self.line_start == self.bytes.len() - 1;
            self.line_start = self.bytes.len();
            if byte == b'\r' {
                self.pending_lf = if empty {
                    PendingLF::AfterFrame
                } else {
                    PendingLF::InFrame
                };
            }
            if empty {
                if byte == b'\r' && self.bytes.len() == MAX_NOTIFICATION_WIRE_BYTES {
                    self.deferred = true;
                } else {
                    return Ok(FrameStep {
                        consumed,
                        complete: true,
                    });
                }
            }
        }
    }

    fn consume_prefix(&mut self, byte: u8) -> Result<bool, ProtocolError> {
        if byte == BOM[self.bom_prefix] {
            self.bom_prefix += 1;
            if self.bom_prefix == BOM.len() {
                self.at_start = false;
                self.bom_prefix = 0;
            }
            return Ok(true);
        }
        self.at_start = false;
        for &prefix in &BOM[..self.bom_prefix] {
            self.push(prefix)?;
        }
        self.bom_prefix = 0;
        Ok(false)
    }

    fn push(&mut self, byte: u8) -> Result<(), ProtocolError> {
        if self.bytes.len() == MAX_NOTIFICATION_WIRE_BYTES {
            return Err(ProtocolError::ByteLimit);
        }
        self.bytes.push(byte);
        Ok(())
    }

    pub fn frame(&self) -> &[u8] {
        &self.bytes
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
        self.line_start = 0;
    }

    pub fn complete_at_end(&mut self) -> bool {
        std::mem::take(&mut self.deferred)
    }

    pub fn consume_terminal_lf(&mut self, input: &[u8]) -> usize {
        if matches!(self.pending_lf, PendingLF::AfterFrame) && input.first() == Some(&b'\n') {
            self.pending_lf = PendingLF::None;
            1
        } else {
            0
        }
    }

    pub fn end_error(&self) -> ProtocolError {
        match std::str::from_utf8(&self.bytes) {
            Err(error) if error.error_len().is_some() => ProtocolError::InvalidUTF8,
            _ => ProtocolError::UnexpectedEnd,
        }
    }
}

pub(super) struct Fields<'a> {
    pub event: &'a str,
    pub data: Cow<'a, str>,
}

/// Follow SSE's first-colon, one-space, last-event-field and joined-data rules; only version one's event/data/comment fields are supported.
pub(super) fn fields(frame: &[u8]) -> Result<Option<Fields<'_>>, ProtocolError> {
    let text = std::str::from_utf8(frame).map_err(|_| ProtocolError::InvalidUTF8)?;
    let mut event = None;
    let mut data: Option<Cow<'_, str>> = None;
    for line in text.split(['\r', '\n']) {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match name {
            "event" => event = Some(value),
            "data" => match &mut data {
                None => data = Some(Cow::Borrowed(value)),
                Some(previous) => {
                    if let Cow::Borrowed(first) = previous {
                        let mut joined = String::new();
                        joined
                            .try_reserve_exact(frame.len())
                            .map_err(|_| ProtocolError::Allocation)?;
                        joined.push_str(first);
                        *previous = Cow::Owned(joined);
                    }
                    let Cow::Owned(joined) = previous else {
                        unreachable!()
                    };
                    joined.push('\n');
                    joined.push_str(value);
                }
            },
            _ => return Err(ProtocolError::InvalidFields),
        }
    }
    match (event, data) {
        (None, None) => Ok(None),
        (Some(event), Some(data)) => Ok(Some(Fields { event, data })),
        _ => Err(ProtocolError::InvalidFields),
    }
}
