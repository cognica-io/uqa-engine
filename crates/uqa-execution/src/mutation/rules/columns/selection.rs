//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, RuleColumnSets};
use crate::mutation::rules::indices::RuleIndexIter;
use crate::mutation::rules::PreparedRuleBatch;
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};
use uqa_sql::SQLError;

struct SelectedColumns {
    indices: RuleIndexIter,
    next: Option<usize>,
    columns: BTreeSet<String>,
}

impl SelectedColumns {
    fn advance(&mut self, previous: usize) -> Result<(), SQLError> {
        self.next = self.indices.next().transpose()?;
        if self.next.is_some_and(|next| next <= previous) {
            return Err(invalid(
                "qualification ordinals are not strictly increasing",
            ));
        }
        Ok(())
    }
}

impl PreparedRuleBatch {
    pub fn matched_action_row_columns(&self) -> Result<RuleColumnSets<1>, SQLError> {
        let mut selected = Vec::new();
        for rule in &self.rules {
            let columns = rule
                .action_row_columns
                .iter()
                .flatten()
                .cloned()
                .collect::<BTreeSet<_>>();
            if columns.is_empty() {
                continue;
            }
            let mut indices = rule.matching_rows.reader()?;
            let next = indices.next().transpose()?;
            selected.push(SelectedColumns {
                indices,
                next,
                columns,
            });
        }
        let mut next_rows: BinaryHeap<_> = selected
            .iter()
            .enumerate()
            .filter_map(|(rule, selected)| selected.next.map(|row| Reverse((row, rule))))
            .collect();
        // Qualification enumerates original events exactly once in order; the generic ordinal owner also supports arbitrary action-time selections.
        let output = RuleColumnSets::from_rows(
            (0..self.rows.len()).map(|index| {
                let mut columns = BTreeSet::new();
                while next_rows.peek().is_some_and(|next| next.0 .0 == index) {
                    let Reverse((_, rule)) = next_rows.pop().expect("peek found a rule");
                    let selection = &mut selected[rule];
                    columns.extend(selection.columns.iter().cloned());
                    selection.advance(index)?;
                    if let Some(row) = selection.next {
                        next_rows.push(Reverse((row, rule)));
                    }
                }
                Ok([columns])
            }),
            self.work_mem,
            self.cancellation.clone(),
        )?;
        if selected.iter().any(|selection| selection.next.is_some()) {
            return Err(invalid("qualification ordinal is outside the event set"));
        }
        Ok(output)
    }

    pub fn missing_action_row_columns(&self) -> Result<RuleColumnSets<2>, SQLError> {
        let matched = self.matched_action_row_columns()?;
        RuleColumnSets::from_rows(
            matched
                .reader()?
                .zip(self.rows.iter())
                .map(|(columns, row)| {
                    let [columns] = columns?;
                    let row = row?;
                    let missing = |record: &Option<uqa_storage::document_store::Document>| {
                        if let Some(record) = record {
                            columns
                                .iter()
                                .filter(|column| !record.contains_key(*column))
                                .cloned()
                                .collect()
                        } else {
                            BTreeSet::new()
                        }
                    };
                    Ok([missing(&row.old), missing(&row.new)])
                }),
            self.work_mem,
            self.cancellation.clone(),
        )
    }
}
