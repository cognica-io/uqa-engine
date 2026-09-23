//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Produced, ProductionControl};
use crate::{memory::BudgetedString, ValueRetentionError};

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
            Buffer::Ordinary(value) => value.reserve(additional),
            Buffer::Controlled(value) => value.reserve(additional)?,
        }
        Ok(())
    }

    pub fn push_str(&mut self, text: &str) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        let mut begin = 0;
        while begin < text.len() {
            self.control.check()?;
            let mut end = begin.saturating_add(4096).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            match &mut self.buffer {
                Buffer::Ordinary(value) => value.push_str(&text[begin..end]),
                Buffer::Controlled(value) => value.push_str(&text[begin..end])?,
            }
            begin = end;
        }
        self.control.check()
    }

    pub fn push(&mut self, character: char) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        match &mut self.buffer {
            Buffer::Ordinary(value) => value.push(character),
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
