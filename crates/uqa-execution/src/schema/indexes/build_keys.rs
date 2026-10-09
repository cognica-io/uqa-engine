//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index builds compare SQL keys in bounded sorted runs before checking uniqueness.

use crate::{
    physical::physical_exec_error, Batch, PhysicalOperator, PhysicalRow, RowSchema, ScalarExpr,
    Sort, SortKey, SpillBuffer, SpillScan,
};
use uqa_core::Value;
use uqa_sql::SQLError;

mod evaluator;

pub(crate) struct IndexBuildKeys {
    rows: SpillBuffer,
    schema: RowSchema,
    relation: uqa_sql::ast::InternalRelationId,
    budget: usize,
    width: usize,
    pushed: i64,
}

impl IndexBuildKeys {
    /// Keys of `width` values; each is stored with the position it was pushed at.
    pub(crate) fn new(width: usize, budget: usize) -> Self {
        let relation = uqa_sql::ast::InternalRelationId::allocate();
        Self {
            rows: SpillBuffer::new(budget),
            schema: RowSchema::with_internal_relation_types(relation, vec![None; width + 1]),
            relation,
            budget,
            width,
            pushed: 0,
        }
    }

    pub(crate) fn push(&mut self, mut values: Vec<Value>) -> Result<(), SQLError> {
        values.push(Value::Int(self.pushed));
        self.pushed += 1;
        self.rows
            .push(Batch::from_physical_rows(
                self.schema.clone(),
                vec![PhysicalRow::from_values(values)],
            ))
            .map_err(physical_exec_error)?;
        Ok(())
    }

    /// Sort the keys, which fails on a comparison that fails, and find the key a unique build reports as duplicated: the key of the first row, in the order the rows were pushed, that repeats the key of an earlier row. A key holding a NULL repeats nothing unless NULLs are not distinct.
    pub(crate) fn first_duplicate(
        self,
        unique: bool,
        nulls_not_distinct: bool,
        catalog: Option<&(dyn uqa_sql::expr::SQLValueCatalog + Send + Sync)>,
    ) -> Result<Option<Vec<Value>>, SQLError> {
        let width = self.width;
        let column = |index| SortKey {
            expr: ScalarExpr::InternalColumn(self.relation.column(index)),
            descending: false,
            nulls_first: Some(false),
        };
        let keys = (0..width).map(column).collect::<Vec<_>>();
        // Rows with equal keys follow one another in the order they were pushed.
        let order = (0..=width).map(column).collect::<Vec<_>>();
        let mut sorted = Sort::with_evaluator_and_work_mem(
            Box::new(SpillScan::new(self.schema, self.rows)),
            order,
            std::sync::Arc::new(evaluator::IndexKeyEvaluator(catalog)),
            self.budget,
        );
        sorted.open().map_err(physical_exec_error)?;
        let mut previous: Option<PhysicalRow> = None;
        let mut repeated = false;
        let mut first: Option<(i64, Vec<Value>)> = None;
        while let Some(batch) = sorted.next().map_err(physical_exec_error)? {
            for row in batch.rows {
                if let Some(previous) = &previous {
                    let ordering = crate::relational::SortComparison {
                        keys: &keys,
                        enums: catalog
                            .map(|catalog| catalog as &dyn uqa_sql::expr::SQLValueCatalog),
                        states: &[],
                        equality_keys: false,
                    }
                    .compare_by(|index| {
                        (
                            previous.value(index).expect("retained index key"),
                            row.value(index).expect("sorted index key"),
                        )
                    })
                    .map_err(physical_exec_error)?;
                    if !ordering.is_eq() {
                        repeated = false;
                    } else if unique
                        && !repeated
                        && (nulls_not_distinct
                            || !(0..width)
                                .any(|index| matches!(row.value(index), Some(Value::Null))))
                    {
                        // The second row of a run of equal keys is the first to repeat that key.
                        repeated = true;
                        let Some(Value::Int(position)) = row.value(width) else {
                            return Err(SQLError::Internal(
                                "index build key lost its position".into(),
                            ));
                        };
                        if first
                            .as_ref()
                            .is_none_or(|(earliest, _)| position < earliest)
                        {
                            first = Some((
                                *position,
                                (0..width)
                                    .map(|index| row.value(index).cloned().unwrap_or(Value::Null))
                                    .collect(),
                            ));
                        }
                    }
                }
                previous = Some(row);
            }
        }
        sorted.close().map_err(physical_exec_error)?;
        Ok(first.map(|(_, key)| key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{ArrayValue, LegacyVectorKind, LegacyVectorValue};

    #[test]
    fn index_builds_check_duplicates_and_null_policy_in_internal_slots() {
        for budget in [1, 1 << 20] {
            for (values, unique, nulls_not_distinct, expected) in [
                (vec![Value::Int(2), Value::Int(1)], true, false, false),
                (vec![Value::Int(1), Value::Int(1)], true, false, true),
                (vec![Value::Int(1), Value::Int(1)], false, false, false),
                (vec![Value::Null, Value::Null], true, false, false),
                (vec![Value::Null, Value::Null], true, true, true),
            ] {
                let mut keys = IndexBuildKeys::new(1, budget);
                for value in values.iter().cloned() {
                    keys.push(vec![value]).unwrap();
                }
                let first = values.first().cloned();
                assert_eq!(
                    keys.first_duplicate(unique, nulls_not_distinct, None)
                        .unwrap(),
                    expected.then(|| vec![first.expect("duplicated key")])
                );
            }
        }
    }

    #[test]
    fn legacy_vector_index_builds_compare_only_when_another_key_exists() {
        let invalid = Value::LegacyVector(
            LegacyVectorValue::try_from_array(
                LegacyVectorKind::Oid,
                ArrayValue::with_lower_bounds(vec![], vec![]).unwrap(),
            )
            .unwrap(),
        );
        for budget in [1, 1 << 20] {
            let mut single = IndexBuildKeys::new(1, budget);
            single.push(vec![invalid.clone()]).unwrap();
            assert_eq!(single.first_duplicate(true, false, None).unwrap(), None);
            for unique in [false, true] {
                for null_prefix in [false, true] {
                    let key = if null_prefix {
                        vec![Value::Null, invalid.clone()]
                    } else {
                        vec![invalid.clone()]
                    };
                    let mut duplicate = IndexBuildKeys::new(key.len(), budget);
                    duplicate.push(key.clone()).unwrap();
                    duplicate.push(key).unwrap();
                    let error = duplicate.first_duplicate(unique, false, None).unwrap_err();
                    assert_eq!(error.sqlstate(), Some("42804"));
                    assert_eq!(error.to_string(), "array is not a valid oidvector");
                }
            }
            let mut distinct_prefix = IndexBuildKeys::new(2, budget);
            for prefix in [1, 0] {
                distinct_prefix
                    .push(vec![Value::Int(prefix), invalid.clone()])
                    .unwrap();
            }
            assert_eq!(
                distinct_prefix.first_duplicate(true, false, None).unwrap(),
                None
            );
        }
    }

    #[test]
    fn a_unique_build_reports_the_first_row_that_repeats_an_earlier_key() {
        for budget in [1, 1 << 20] {
            for (values, nulls_not_distinct, expected) in [
                (vec![Some(2), Some(2), Some(1), Some(1)], false, Some(2)),
                (vec![Some(2), Some(1), Some(1), Some(2)], false, Some(1)),
                (
                    vec![Some(3), Some(1), Some(2), Some(3), Some(1)],
                    false,
                    Some(3),
                ),
                (vec![None, None, Some(1), Some(1)], false, Some(1)),
                (vec![Some(1), Some(1), Some(1)], false, Some(1)),
            ] {
                let mut keys = IndexBuildKeys::new(1, budget);
                for value in values {
                    keys.push(vec![value.map_or(Value::Null, Value::Int)])
                        .unwrap();
                }
                assert_eq!(
                    keys.first_duplicate(true, nulls_not_distinct, None)
                        .unwrap(),
                    expected.map(|value| vec![Value::Int(value)])
                );
            }
            let mut keys = IndexBuildKeys::new(1, budget);
            for value in [Value::Int(1), Value::Null, Value::Null, Value::Int(1)] {
                keys.push(vec![value]).unwrap();
            }
            assert_eq!(
                keys.first_duplicate(true, true, None).unwrap(),
                Some(vec![Value::Null])
            );
        }
    }
}
