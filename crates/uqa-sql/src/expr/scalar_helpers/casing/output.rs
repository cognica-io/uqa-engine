//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A bounded stack buffer in front of a mapping's admitted destination, so the destination admits output and observes both cancellation scopes once per buffered chunk rather than once per character.

use uqa_core::{memory::ProductionString, ValueRetentionError};
use writeable::Writeable;

/// Mapped bytes held before they are admitted to the destination.
const CAPACITY: usize = 1024;

pub(super) struct MappedOutput<'o, 'c> {
    output: &'o mut ProductionString<'c>,
    buffer: [u8; CAPACITY],
    length: usize,
    /// The admission or cancellation failure an ICU4X writer reported only as `fmt::Error`.
    error: Option<ValueRetentionError>,
}

impl<'o, 'c> MappedOutput<'o, 'c> {
    pub(super) fn new(output: &'o mut ProductionString<'c>) -> Self {
        Self {
            output,
            buffer: [0; CAPACITY],
            length: 0,
            error: None,
        }
    }

    pub(super) fn push(&mut self, character: char) -> Result<(), ValueRetentionError> {
        let width = character.len_utf8();
        if width > CAPACITY - self.length {
            self.flush()?;
        }
        character.encode_utf8(&mut self.buffer[self.length..self.length + width]);
        self.length += width;
        Ok(())
    }

    pub(super) fn push_str(&mut self, text: &str) -> Result<(), ValueRetentionError> {
        if text.len() > CAPACITY - self.length {
            self.flush()?;
            if text.len() > CAPACITY {
                return self.output.push_str(text);
            }
        }
        self.buffer[self.length..self.length + text.len()].copy_from_slice(text.as_bytes());
        self.length += text.len();
        Ok(())
    }

    /// Write an ASCII run with every letter mapped to uppercase or lowercase, which every provider's mapping of ASCII text agrees with.
    pub(super) fn push_ascii(
        &mut self,
        run: &str,
        uppercase: bool,
    ) -> Result<(), ValueRetentionError> {
        debug_assert!(run.is_ascii());
        for piece in run.as_bytes().chunks(CAPACITY) {
            if piece.len() > CAPACITY - self.length {
                self.flush()?;
            }
            let target = &mut self.buffer[self.length..self.length + piece.len()];
            target.copy_from_slice(piece);
            if uppercase {
                target.make_ascii_uppercase();
            } else {
                target.make_ascii_lowercase();
            }
            self.length += piece.len();
        }
        Ok(())
    }

    /// Write an ICU4X mapping of a whole run.
    pub(super) fn write(&mut self, mapped: &impl Writeable) -> Result<(), ValueRetentionError> {
        let written = mapped.write_to(self);
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        assert!(
            written.is_ok(),
            "a case-mapping writer failed without an admission error"
        );
        Ok(())
    }

    /// Admit the buffered bytes; the caller flushes before it finishes the destination.
    pub(super) fn flush(&mut self) -> Result<(), ValueRetentionError> {
        if self.length > 0 {
            let text = std::str::from_utf8(&self.buffer[..self.length])
                .expect("the buffer holds whole characters");
            self.output.push_str(text)?;
            self.length = 0;
        }
        Ok(())
    }
}

impl std::fmt::Write for MappedOutput<'_, '_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.push_str(text).map_err(|error| {
            self.error = Some(error);
            std::fmt::Error
        })
    }

    fn write_char(&mut self, character: char) -> std::fmt::Result {
        self.push(character).map_err(|error| {
            self.error = Some(error);
            std::fmt::Error
        })
    }
}
