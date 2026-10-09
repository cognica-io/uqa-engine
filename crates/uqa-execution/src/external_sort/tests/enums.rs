//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{ArrayValue, DatumValue, EnumLabelKey, EnumValue};
use uqa_sql::expr::enums::{
    EnumComparisonState, EnumLabelCatalog, EnumTypeComparisonStates, EnumTypeLabels,
};
use uqa_sql::SQLError;

#[derive(Default)]
struct Catalog {
    states: EnumTypeComparisonStates,
}

impl EnumLabelCatalog for Catalog {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok(matches!(oid, 16_384 | 16_400).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: Vec::new(),
            })
        }))
    }
    fn enum_label_position(&self, oid: u32) -> Result<Option<(u32, usize)>, SQLError> {
        Ok(match oid {
            2 => Some((16_384, 0)),
            5 => Some((16_384, 1)),
            4 => Some((16_384, 2)),
            11 => Some((16_400, 0)),
            8 => Some((16_400, 1)),
            _ => None,
        })
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, oid: u32) -> Result<Option<String>, SQLError> {
        Ok(Some(
            if oid == 16_384 {
                "first_enum"
            } else {
                "second_enum"
            }
            .into(),
        ))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
    fn enum_type_comparison_states(&self) -> Option<&EnumTypeComparisonStates> {
        Some(&self.states)
    }
}

impl crate::ExpressionEvaluator for Catalog {
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        Some(self)
    }
    fn evaluate(
        &self,
        expression: &ScalarExpr,
        row: &dyn uqa_sql::expr::RowLookup,
    ) -> ExecResult<Value> {
        match expression {
            ScalarExpr::Column(name) => Ok(row.column(name).cloned().unwrap_or(Value::Null)),
            ScalarExpr::InternalColumn(column) => {
                Ok(row.internal_column(*column).cloned().unwrap_or(Value::Null))
            }
            _ => Err(ExecError::Other("expected test column".into())),
        }
    }
}

fn physical(oid: u32) -> Value {
    Value::Datum(DatumValue::new(16_400, 0, oid.to_le_bytes().to_vec()))
}

fn key() -> SortKey {
    SortKey {
        expr: ScalarExpr::Column("key".into()),
        descending: false,
        nulls_first: None,
    }
}

#[test]
fn enum_sort_keeps_catalog_through_runs_merge_top_k_and_nested_keys() {
    for budget in [0, 4096] {
        for keep in [None, Some(2)] {
            for shape in 0..3 {
                let rows = (0..40)
                    .map(|id| {
                        let value = physical(if id % 2 == 0 { 5 } else { 4 });
                        let value = match shape {
                            0 => value,
                            1 => Value::Array(ArrayValue::try_new(vec![value]).unwrap()),
                            _ => Value::Record(vec![("e".into(), value)]),
                        };
                        ResultRow::from([("key".into(), value), ("input".into(), Value::Int(id))])
                    })
                    .collect();
                let mut sort = ExternalSort::new(
                    Box::new(TableScan::from_rows(
                        vec!["key".into(), "input".into()],
                        rows,
                    )),
                    vec![key()],
                    Arc::new(Catalog::default()),
                    keep,
                    budget,
                );
                let (_, rows) = run_to_rows(&mut sort).unwrap();
                let expected = (0..40)
                    .step_by(2)
                    .chain((1..40).step_by(2))
                    .take(keep.unwrap_or(40))
                    .collect::<Vec<_>>();
                assert_eq!(int_column(&rows, "input"), expected);
                if budget == 0 {
                    assert!(sort.initial_run_count() > EXTERNAL_SORT_MERGE_FAN_IN);
                    assert!(sort.merge_pass_count() > 1);
                }
            }
        }
    }
}

#[test]
fn ordering_call_state_is_private_while_tie_equality_reads_raw_oids() {
    let catalog = Catalog::default();
    let keys = [key()];
    let states = [EnumComparisonState::default()];
    let compare = SortComparison {
        keys: &keys,
        enums: Some(&catalog),
        states: &states,
        equality_keys: false,
    };
    let first_left = physical(5);
    let first_right = physical(4);
    assert_eq!(
        compare.compare_by(|_| (&first_left, &first_right)).unwrap(),
        Ordering::Less
    );
    let left = physical(11);
    let right = physical(8);
    let error = compare.compare_by(|_| (&left, &right)).unwrap_err();
    assert!(error.to_string().contains("first_enum"));
    let fresh = [EnumComparisonState::default()];
    assert_eq!(
        SortComparison {
            states: &fresh,
            ..compare
        }
        .compare_by(|_| (&left, &right))
        .unwrap(),
        Ordering::Less
    );
    assert!(crate::relational::equal_sort_key_values(
        &[physical(1)],
        &[physical(1)],
        Some(&catalog)
    )
    .unwrap());
    assert!(!crate::relational::equal_sort_key_values(
        &[physical(1)],
        &[physical(3)],
        Some(&catalog)
    )
    .unwrap());
}

#[test]
fn enum_distinct_keys_share_physical_identity_across_spill_and_carriers() {
    for budget in [0, 4096] {
        let fresh = Value::Enum(
            EnumValue::new(16_400, EnumLabelKey::initial(1).unwrap().remove(0))
                .with_label_oid(Some(5)),
        );
        let rows = [physical(5), fresh, physical(1), physical(1)]
            .into_iter()
            .map(|value| ResultRow::from([("key".into(), value)]))
            .collect();
        let mut distinct = crate::Distinct::all_with_work_mem(
            Box::new(TableScan::from_rows(vec!["key".into()], rows)),
            budget,
        )
        .with_evaluator(Arc::new(Catalog::default()));
        assert_eq!(run_to_rows(&mut distinct).unwrap().1.len(), 2);
    }
}
