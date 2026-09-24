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

pub(super) struct IndexBuildKeys {
    rows: SpillBuffer,
    schema: RowSchema,
    relation: uqa_sql::ast::InternalRelationId,
    budget: usize,
}

impl IndexBuildKeys {
    pub(super) fn new(width: usize, budget: usize) -> Self {
        let relation = uqa_sql::ast::InternalRelationId::allocate();
        Self {
            rows: SpillBuffer::new(budget),
            schema: RowSchema::with_internal_relation_types(relation, vec![None; width]),
            relation,
            budget,
        }
    }

    pub(super) fn push(&mut self, values: Vec<Value>) -> Result<(), SQLError> {
        self.rows
            .push(Batch::from_physical_rows(
                self.schema.clone(),
                vec![PhysicalRow::from_values(values)],
            ))
            .map_err(physical_exec_error)?;
        Ok(())
    }

    pub(super) fn validate(
        self,
        name: &str,
        unique: bool,
        nulls_not_distinct: bool,
    ) -> Result<(), SQLError> {
        let keys = (0..self.schema.physical_width())
            .map(|index| SortKey {
                expr: ScalarExpr::InternalColumn(self.relation.column(index)),
                descending: false,
                nulls_first: Some(false),
            })
            .collect::<Vec<_>>();
        let mut sorted = Sort::with_work_mem(
            Box::new(SpillScan::new(self.schema, self.rows)),
            keys.clone(),
            vec![],
            self.budget,
        );
        sorted.open().map_err(physical_exec_error)?;
        let mut previous: Option<PhysicalRow> = None;
        while let Some(batch) = sorted.next().map_err(physical_exec_error)? {
            for row in batch.rows {
                if let Some(previous) = &previous {
                    let ordering = crate::relational::compare_sort_key_values_by(&keys, |index| {
                        (
                            previous.value(index).expect("retained index key"),
                            row.value(index).expect("sorted index key"),
                        )
                    })
                    .map_err(physical_exec_error)?;
                    if unique
                        && ordering.is_eq()
                        && (nulls_not_distinct
                            || !(0..keys.len())
                                .any(|index| matches!(row.value(index), Some(Value::Null))))
                    {
                        return Err(SQLError::Routine {
                            sqlstate: "23505".into(),
                            message: format!(
                                r#"could not create unique index "{name}": key is duplicated"#
                            ),
                        });
                    }
                }
                previous = Some(row);
            }
        }
        sorted.close().map_err(physical_exec_error)
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
                (vec![Value::Int(2), Value::Int(1)], true, false, None),
                (
                    vec![Value::Int(1), Value::Int(1)],
                    true,
                    false,
                    Some("23505"),
                ),
                (vec![Value::Int(1), Value::Int(1)], false, false, None),
                (vec![Value::Null, Value::Null], true, false, None),
                (vec![Value::Null, Value::Null], true, true, Some("23505")),
            ] {
                let mut keys = IndexBuildKeys::new(1, budget);
                for value in values {
                    keys.push(vec![value]).unwrap();
                }
                match (keys.validate("keys", unique, nulls_not_distinct), expected) {
                    (Ok(()), None) => (),
                    (Err(error), Some(expected)) => assert_eq!(error.sqlstate(), Some(expected)),
                    (result, expected) => panic!("{result:?}, expected {expected:?}"),
                }
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
            single.validate("one_key", true, false).unwrap();
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
                    let error = duplicate.validate("two_keys", unique, false).unwrap_err();
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
            distinct_prefix
                .validate("prefix_first", true, false)
                .unwrap();
        }
    }
}
