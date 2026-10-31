//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original-message position indexes retain their admitted buffers with every shared parser statement.

use super::SourcePositions;
use crate::SQLError;
use std::sync::Arc;
use uqa_core::memory::{ProductionControl, ProductionVec};

impl SourcePositions {
    pub(super) fn new_with_control(
        sql: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut offsets = ProductionVec::new(*control);
        let mut count = 0usize;
        for character in sql.chars() {
            control.check()?;
            count += usize::from(character.len_utf8() != 1);
        }
        offsets.reserve(count)?;
        let mut excess = 0usize;
        for (offset, character) in sql.char_indices() {
            control.check()?;
            let width = character.len_utf8();
            if width != 1 {
                excess += width - 1;
                offsets.push_copy((offset + width, excess))?;
            }
        }
        let (excess_bytes, memory) = offsets.finish()?.into_parts();
        Ok(Self {
            length: sql.len(),
            excess_bytes,
            memory,
        })
    }

    pub(in crate::compiler) fn shared_with_control(
        sql: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<Self>, SQLError> {
        let mut positions = if control.budget().is_some() {
            Self::new_with_control(sql, control)?
        } else {
            Self::new(sql)
        };
        let shared = control.reserve(size_of::<Self>())?;
        positions.memory = control.combine(positions.memory.take(), shared);
        let positions = Arc::new(positions);
        control.check()?;
        Ok(positions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn shared_position_index_retains_one_exact_payload_until_the_last_statement() {
        let text = "SELECT '🙂한글'; SELECT value";
        let budget = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut index = None;
        let allocation = allocation_counter::measure(|| {
            index = Some(SourcePositions::shared_with_control(text, &control).unwrap());
        });
        let index = index.unwrap();
        // Arc's two reference counts are allocator bookkeeping, outside the owned payload contract.
        assert_eq!(
            allocation.bytes_current,
            i64::try_from(budget.used() + 2 * size_of::<usize>()).unwrap()
        );
        for (character, (byte, _)) in text.char_indices().enumerate() {
            assert_eq!(
                index.position(i32::try_from(byte).unwrap()),
                Some(u32::try_from(character + 1).unwrap())
            );
        }
        let last = Arc::clone(&index);
        let held = budget.used();
        drop(index);
        assert_eq!(budget.used(), held);
        drop(last);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn position_index_rejects_large_buffers_and_both_cancellations_before_publication() {
        let text = "🙂".repeat(4096);
        let budget = MemoryBudget::new(256);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            SourcePositions::shared_with_control(&text, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 0);
        for original_cancelled in [true, false] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let control = ProductionControl::new(&budget, &original, &invoking);
            assert!(control.check().is_err());
            let allocation = allocation_counter::measure(|| {
                assert_eq!(
                    SourcePositions::shared_with_control(&text, &control)
                        .unwrap_err()
                        .sqlstate(),
                    Some("57014")
                );
            });
            assert_eq!(allocation.count_total, 0);
            assert_eq!(budget.used(), 0);
        }
    }
}
