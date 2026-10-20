//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sort temporal parent ranges in bounded runs and observe their union without retaining it.

use crate::physical::physical_exec_error;
use crate::{
    Batch, PhysicalOperator, PhysicalRow, RowSchema, ScalarExpr, SharedSpillScan, Sort, SortKey,
    SpillBuffer,
};
use uqa_core::{CancellationToken, Value};
use uqa_sql::ast::{ColumnType, InternalRelationId, RangeSubtype};
use uqa_sql::expr::{parse_range, CanonicalRange};
use uqa_sql::SQLError;

pub(super) struct ParentRanges {
    rows: SpillBuffer,
    schema: RowSchema,
    relation: InternalRelationId,
    subtype: RangeSubtype,
    bytes: usize,
    cancellation: CancellationToken,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct RangeObservation {
    pub covered: bool,
    pub overlaps: bool,
}

impl ParentRanges {
    pub(super) fn new(
        subtype: RangeSubtype,
        bytes: usize,
        cancellation: CancellationToken,
    ) -> Self {
        let relation = InternalRelationId::allocate();
        Self {
            rows: SpillBuffer::new(bytes),
            schema: RowSchema::with_internal_relation_types(
                relation,
                vec![Some(ColumnType::Range(subtype))],
            ),
            relation,
            subtype,
            bytes,
            cancellation,
        }
    }

    pub(super) fn push(&mut self, range: &CanonicalRange) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if range.subtype() != self.subtype {
            return Err(SQLError::TypeMismatch(
                "PERIOD foreign-key range subtypes do not match".into(),
            ));
        }
        if !range.is_empty() {
            self.rows
                .push(Batch::from_physical_rows(
                    self.schema.clone(),
                    vec![PhysicalRow::from_values(vec![Value::Str(range.to_text())])],
                ))
                .map_err(physical_exec_error)?;
        }
        Ok(())
    }

    pub(super) fn observe(self, children: &[CanonicalRange]) -> Result<RangeObservation, SQLError> {
        self.cancellation.check()?;
        let mut sorted = Sort::with_evaluator_and_work_mem(
            Box::new(SharedSpillScan::new(
                self.rows
                    .into_shared(self.schema)
                    .map_err(physical_exec_error)?,
            )),
            vec![SortKey {
                expr: ScalarExpr::InternalColumn(self.relation.column(0)),
                descending: false,
                nulls_first: Some(false),
            }],
            std::sync::Arc::new(RangeEvaluator(self.cancellation.clone())),
            // The merge retains one decoded batch per input plus output scratch. Share this owner's allowance across them instead of granting each reader the full amount.
            self.bytes / (crate::external_sort::EXTERNAL_SORT_MERGE_FAN_IN + 2),
        );
        sorted.open().map_err(physical_exec_error)?;
        let result = observe_sorted(&mut sorted, self.subtype, children, &self.cancellation);
        let closed = sorted.close().map_err(physical_exec_error);
        result.and_then(|observation| closed.map(|()| observation))
    }
}

fn observe_sorted(
    sorted: &mut Sort<'_>,
    subtype: RangeSubtype,
    children: &[CanonicalRange],
    cancellation: &CancellationToken,
) -> Result<RangeObservation, SQLError> {
    let mut union: Option<CanonicalRange> = None;
    let mut coverage = Coverage::new(children);
    while let Some(batch) = sorted.next().map_err(physical_exec_error)? {
        for row in batch.rows {
            cancellation.check()?;
            let Some(Value::Str(text)) = row.value(0) else {
                return Err(SQLError::Internal(
                    "PERIOD range spill lost its canonical value".into(),
                ));
            };
            let range = parse_range(text, subtype)?;
            if let Some(previous) = union.take() {
                if previous.overlaps(&range) || previous.adjacent(&range) {
                    union = Some(previous.merge_cover(&range));
                } else {
                    coverage.observe(&previous)?;
                    union = Some(range);
                }
            } else {
                union = Some(range);
            }
        }
    }
    cancellation.check()?;
    if let Some(range) = union {
        coverage.observe(&range)?;
    }
    Ok(RangeObservation {
        covered: !coverage.gap && coverage.next == children.len(),
        overlaps: coverage.overlaps,
    })
}

/// Child multiranges and completed parent components are canonical and ordered. Both cursors advance monotonically.
struct Coverage<'a> {
    children: &'a [CanonicalRange],
    next: usize,
    overlap_next: usize,
    gap: bool,
    overlaps: bool,
}

impl<'a> Coverage<'a> {
    const fn new(children: &'a [CanonicalRange]) -> Self {
        Self {
            children,
            next: 0,
            overlap_next: 0,
            gap: false,
            overlaps: false,
        }
    }

    fn observe(&mut self, parent: &CanonicalRange) -> Result<(), SQLError> {
        let control = uqa_core::memory::ProductionControl::uncontrolled();
        while !self.gap {
            let Some(child) = self.children.get(self.next) else {
                break;
            };
            if parent.contains_range(child) {
                self.next += 1;
            } else if !parent.overlaps(child) && parent.cmp_with_control(child, &control)?.is_lt() {
                break;
            } else {
                self.gap = true;
            }
        }
        while !self.overlaps {
            let Some(child) = self.children.get(self.overlap_next) else {
                break;
            };
            if parent.overlaps(child) {
                self.overlaps = true;
            } else if parent.cmp_with_control(child, &control)?.is_lt() {
                break;
            } else {
                self.overlap_next += 1;
            }
        }
        Ok(())
    }
}

struct RangeEvaluator(CancellationToken);

impl crate::ExpressionEvaluator for RangeEvaluator {
    fn evaluate(
        &self,
        expression: &ScalarExpr,
        row: &dyn uqa_sql::expr::RowLookup,
    ) -> crate::ExecResult<Value> {
        self.0.check().map_err(SQLError::from)?;
        Ok(crate::eval_scalar(
            expression,
            &crate::ScalarEvalContext::from_row_lookup(row, &[]),
        )?)
    }
}

#[cfg(test)]
mod tests;
