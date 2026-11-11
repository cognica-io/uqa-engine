//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored column input transfers its original native allocations into a shared generation.

use super::{MemoryBudget, ProductionControl, TableDefinition};
use crate::{ast::ColumnDef, SQLError};
use uqa_core::CancellationToken;

impl TableDefinition<Vec<ColumnDef>> {
    /// Decode stored column JSON directly into a native immutable generation. Complete syntax and EOF validation precede publication; original payload allowances move with the decoded values. No expression, type-input routine or resource callback is executed.
    pub fn read_stored_json(
        text: &str,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> Result<Self, SQLError> {
        let value = crate::ast::read_stored_columns_json(text, memory, original, invoking)?;
        let value = value
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("stored column input retains its allowance"));
        // Stored JSON admits exclusively owned native values, with no retained query source or separately owned literal generation.
        Self::from_decoded_parts(
            value,
            0,
            &ProductionControl::new(memory, original, invoking),
        )
    }
}
