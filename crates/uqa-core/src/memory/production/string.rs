//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Produced, ProductionControl};
use crate::{
    memory::{BudgetedString, MemoryError},
    ValueRetentionError,
};

enum Buffer {
    Ordinary(String),
    Controlled(BudgetedString),
}

/// A string constructor that admits replacements through the existing budgeted buffer and checks both cancellation owners between bounded UTF-8 chunks.
pub struct ProductionString<'a> {
    buffer: Buffer,
    control: ProductionControl<'a>,
    format_error: Option<ValueRetentionError>,
}

impl<'a> ProductionString<'a> {
    pub fn new(control: ProductionControl<'a>) -> Self {
        let buffer = control.budget().map_or_else(
            || Buffer::Ordinary(String::new()),
            |budget| Buffer::Controlled(BudgetedString::new(budget)),
        );
        Self {
            buffer,
            control,
            format_error: None,
        }
    }

    /// Resume construction using an admitted string's existing buffer and allowance.
    pub fn from_produced(
        value: Produced<String>,
        control: ProductionControl<'a>,
    ) -> Result<Self, ValueRetentionError> {
        control.assert_owner(value.memory.as_ref());
        control.check()?;
        let buffer = match value.into_budgeted() {
            Ok(value) => Buffer::Controlled(BudgetedString::from_budgeted(value)),
            Err(value) => Buffer::Ordinary(
                value
                    .into_uncontrolled()
                    .expect("ordinary string owner was checked"),
            ),
        };
        Ok(Self {
            buffer,
            control,
            format_error: None,
        })
    }

    pub fn truncate(&mut self, length: usize) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        match &mut self.buffer {
            Buffer::Ordinary(value) => value.truncate(length),
            Buffer::Controlled(value) => value.truncate(length),
        }
        Ok(())
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        match &mut self.buffer {
            Buffer::Ordinary(value) => value.try_reserve(additional).map_err(MemoryError::from)?,
            Buffer::Controlled(value) => value.reserve(additional)?,
        }
        Ok(())
    }

    pub fn push_str(&mut self, text: &str) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        let mut begin = 0;
        while begin < text.len() {
            begin += self.push_str_chunk(&text[begin..])?;
        }
        self.control.check()
    }

    /// Largest byte count copied by one native text-construction step.
    pub const CHUNK_BYTES: usize = 4096;

    /// Borrow one complete UTF-8 chunk for a producer that already admitted its destination buffer.
    pub fn first_chunk(text: &str) -> &str {
        let mut end = text.len().min(Self::CHUNK_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }

    /// Append at most 4096 bytes ending at a complete UTF-8 character and return the consumed byte count. Inline and scheduled producers share this admission and copying step.
    pub fn push_str_chunk(&mut self, text: &str) -> Result<usize, ValueRetentionError> {
        self.control.check()?;
        let chunk = Self::first_chunk(text);
        match &mut self.buffer {
            Buffer::Ordinary(value) => {
                value.try_reserve(chunk.len()).map_err(MemoryError::from)?;
                value.push_str(chunk);
            }
            Buffer::Controlled(value) => value.push_str(chunk)?,
        }
        self.control.check()?;
        Ok(chunk.len())
    }

    pub fn push(&mut self, character: char) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        match &mut self.buffer {
            Buffer::Ordinary(value) => {
                value
                    .try_reserve(character.len_utf8())
                    .map_err(MemoryError::from)?;
                value.push(character);
            }
            Buffer::Controlled(value) => value.push(character)?,
        }
        Ok(())
    }

    pub fn finish(self) -> Result<Produced<String>, ValueRetentionError> {
        if let Some(error) = self.format_error {
            return Err(error);
        }
        let (value, memory) = match self.buffer {
            Buffer::Ordinary(value) => (value, None),
            Buffer::Controlled(value) => {
                let (value, memory) = value.into_parts();
                (value, Some(memory))
            }
        };
        self.control.finish(value, memory)
    }
}

impl std::fmt::Write for ProductionString<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.format_error.is_some() {
            return Err(std::fmt::Error);
        }
        self.push_str(text).map_err(|error| {
            self.format_error = Some(error);
            std::fmt::Error
        })
    }
}

impl std::ops::Deref for ProductionString<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        match &self.buffer {
            Buffer::Ordinary(value) => value,
            Buffer::Controlled(value) => value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn string_chunks_share_utf8_boundaries_and_admission_with_inline_copying() {
        let source = format!("{}€🙂{}", "a".repeat(4095), "δ".repeat(7000));
        let native = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&native, &original, &invoking);
        let expected = control.copy_text(&source).unwrap();
        let mut output = ProductionString::new(control);
        output.reserve(source.len()).unwrap();
        let mut offset = 0;
        while offset < source.len() {
            let copied = output.push_str_chunk(&source[offset..]).unwrap();
            assert!((1..=4096).contains(&copied));
            if offset == 0 {
                assert_eq!(copied, 4095);
            }
            offset += copied;
            assert!(source.is_char_boundary(offset));
            assert_eq!(&*output, &source[..offset]);
        }
        assert_eq!(output.push_str_chunk("").unwrap(), 0);
        let output = output.finish().unwrap();
        assert_eq!(*output, *expected);
        assert_eq!(output.reserved_bytes(), expected.reserved_bytes());
        drop((output, expected));
        assert_eq!(native.used(), 0);
        let exhausted = MemoryBudget::new(0);
        let denied = ProductionControl::new(&exhausted, &original, &invoking);
        assert!(ProductionString::new(denied).push_str_chunk("é").is_err());
        for cancel_original in [true, false] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(matches!(
                ProductionString::new(control).push_str_chunk(""),
                Err(ValueRetentionError::Cancelled(_))
            ));
        }
        assert_eq!(native.used(), 0);
    }

    #[test]
    fn resumed_string_keeps_capacity_and_releases_on_both_cancellations() {
        let budget = MemoryBudget::new(64);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let value = control.copy_text("é padded").unwrap();
        let pointer = value.as_ptr();
        let capacity = value.capacity();
        let mut output = ProductionString::from_produced(value, control).unwrap();
        output.truncate("é".len()).unwrap();
        output.push_str("  ").unwrap();
        let output = output.finish().unwrap();
        assert_eq!(output.as_ptr(), pointer);
        assert_eq!(output.capacity(), capacity);
        assert_eq!(budget.used(), capacity);
        drop(output);
        assert_eq!(budget.used(), 0);
        for cancel_original in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let value = control.copy_text("held").unwrap();
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(matches!(
                ProductionString::from_produced(value, control),
                Err(ValueRetentionError::Cancelled(_))
            ));
            assert_eq!(budget.used(), 0);
        }
    }
}
