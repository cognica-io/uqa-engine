//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same SQL key support state follows rows through every external sort run.

use super::{
    DecoratedRow, ExecError, ExecResult, ExternalSort, Ordering, PhysicalOperator, PhysicalRow,
    RowSchema, SortComparison, SortComparisonDomain, SortKey, Value,
};

pub(super) fn compare_records(
    keys: &SortComparison<'_>,
    source_width: usize,
    left: &DecoratedRow,
    right: &DecoratedRow,
) -> ExecResult<Ordering> {
    Ok(keys
        .compare_by(|index| {
            (
                left.row
                    .value(source_width + index)
                    .expect("validated external sort run key"),
                right
                    .row
                    .value(source_width + index)
                    .expect("validated external sort run key"),
            )
        })?
        .then_with(|| left.sequence.cmp(&right.sequence)))
}

impl ExternalSort<'_> {
    pub(super) fn comparison(&self) -> SortComparison<'_> {
        SortComparison {
            keys: &self.keys,
            types: &self.key_types,
            enums: self.evaluator.enum_labels(),
            states: &self.comparison_states,
            domain: self.comparison_domain,
            cancellation: self.cancellation.as_ref(),
        }
    }
}

impl<'a> ExternalSort<'a> {
    /// Internal stored-key construction uses Core ordering, including NULL and representation identities. This path performs no SQL binding, evaluation or operator lookup and advertises no SQL ordering.
    pub(crate) fn physical_columns(
        child: Box<dyn PhysicalOperator + 'a>,
        positions: Vec<usize>,
        work_mem_bytes: usize,
        cancellation: &uqa_core::CancellationToken,
    ) -> ExecResult<Self> {
        if positions
            .iter()
            .any(|&position| position >= child.row_schema().physical_width())
        {
            return Err(ExecError::Other("physical sort key outside row".into()));
        }
        let keys = positions
            .iter()
            .map(|_| SortKey {
                expr: uqa_sql::ir::ScalarExpr::Literal(Value::Null),
                descending: false,
                nulls_first: Some(true),
            })
            .collect();
        let mut sort = Self::new(
            child,
            keys,
            crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
            None,
            work_mem_bytes,
        );
        sort.comparison_domain = SortComparisonDomain::Physical;
        sort.physical_key_slots = positions;
        sort.cancellation = Some(cancellation.clone());
        sort.ordering.clear();
        Ok(sort)
    }

    pub(super) fn capture_keys(
        &self,
        schema: &RowSchema,
        row: &PhysicalRow,
    ) -> ExecResult<Vec<Value>> {
        if let Some(cancellation) = &self.cancellation {
            cancellation.check().map_err(uqa_sql::SQLError::Cancelled)?;
        }
        if self.comparison_domain == SortComparisonDomain::Physical {
            return self
                .physical_key_slots
                .iter()
                .map(|&slot| {
                    row.value(slot)
                        .cloned()
                        .ok_or_else(|| ExecError::Other("physical sort key outside row".into()))
                })
                .collect();
        }
        self.keys
            .iter()
            .map(|key| {
                self.evaluator.evaluate_physical_with_function_states(
                    &key.expr,
                    schema,
                    row,
                    self.keys.calls(),
                )
            })
            .collect()
    }
}
