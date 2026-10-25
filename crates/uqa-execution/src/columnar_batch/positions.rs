//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public result positions preserve repeated labels and the caller's scratch allowance.

use crate::RowSchema;
use std::collections::BTreeMap;
use uqa_core::memory::{BudgetedSharedMap, Produced, ProductionControl, ProductionVec};
use uqa_sql::SQLError;

pub(crate) fn result_column_positions(
    schema: &RowSchema,
    columns: &[String],
) -> Vec<Option<usize>> {
    result_column_positions_with_control(schema, columns, &ProductionControl::uncontrolled())
        .expect("ordinary result column selection")
        .into_uncontrolled()
        .expect("ordinary result column owner")
}

pub(crate) fn result_column_positions_with_control(
    schema: &RowSchema,
    columns: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<usize>>>, SQLError> {
    let mut positions = ProductionVec::new(*control);
    positions.reserve(columns.len())?;
    let mut ordinary = BTreeMap::<&str, usize>::new();
    let mut controlled = control.budget().map(BudgetedSharedMap::<&str, usize>::new);
    let identity = columns == schema.columns();
    for (index, name) in columns.iter().enumerate() {
        control.check()?;
        if identity {
            positions.push_copy(Some(index))?;
            continue;
        }
        let mut occurrence = if let Some(counts) = &mut controlled {
            let occurrence = counts.get(name.as_str()).copied().unwrap_or(0);
            counts.try_insert(name, occurrence + 1)?;
            occurrence
        } else {
            let occurrence = ordinary.entry(name).or_default();
            let previous = *occurrence;
            *occurrence += 1;
            previous
        };
        let mut found = None;
        for (position, input) in schema.columns().iter().enumerate() {
            control.check()?;
            if input == name {
                if occurrence == 0 {
                    found = Some(position);
                    break;
                }
                occurrence -= 1;
            }
        }
        positions.push_copy(found)?;
    }
    Ok(positions.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn repeated_and_missing_labels_keep_their_positions_under_the_original_allowance() {
        let schema = RowSchema::new(["a", "hidden", "a", "b"].map(str::to_owned).to_vec());
        let columns = ["a", "b", "a", "missing", "a"].map(str::to_owned);
        let expected = [Some(0), Some(3), Some(2), None, None];
        assert_eq!(result_column_positions(&schema, &columns), expected);
        for limit in [0, 16, 64, 256, 4096] {
            let memory = MemoryBudget::new(limit);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            match result_column_positions_with_control(&schema, &columns, &control) {
                Ok(positions) => {
                    assert_eq!(*positions, expected);
                    assert_eq!(memory.used(), positions.reserved_bytes());
                }
                Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
            }
            assert_eq!(memory.used(), 0);
            for token in [&original, &invoking] {
                token.cancel();
                assert_eq!(
                    result_column_positions_with_control(&schema, &columns, &control)
                        .unwrap_err()
                        .sqlstate(),
                    Some("57014")
                );
                token.reset();
            }
        }
    }
}
