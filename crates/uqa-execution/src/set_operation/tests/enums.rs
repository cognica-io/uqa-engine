//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{ArrayValue, DatumValue};
use uqa_sql::expr::enums::{EnumLabelCatalog, EnumTypeLabels};
use uqa_sql::SQLError;

struct Catalog;

impl EnumLabelCatalog for Catalog {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok((oid == 16_384).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: Vec::new(),
            })
        }))
    }
    fn enum_label_position(&self, _: u32) -> Result<Option<(u32, usize)>, SQLError> {
        panic!("equality grouping must not inspect label order")
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        panic!("equality grouping must not invoke enum input")
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        panic!("raw OID equality needs no label diagnostic")
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

impl ExpressionEvaluator for Catalog {
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        Some(self)
    }
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        ColumnEvaluator.evaluate(expression, row)
    }
}

fn value(oid: Option<u32>, shape: usize) -> Value {
    let value = oid.map_or(Value::Null, |oid| {
        Value::Datum(DatumValue::new(16_384, 0, oid.to_le_bytes().to_vec()))
    });
    match shape {
        0 => value,
        1 => Value::Array(ArrayValue::try_new(vec![value]).unwrap()),
        _ => Value::Record(vec![("e".into(), value)]),
    }
}

#[test]
fn spilled_set_operations_count_raw_enum_identities_without_order_or_output() {
    for budget in [1, 4096] {
        for shape in 0..3 {
            for (kind, all, expected) in [
                (SetOpKind::Union, false, [1, 1, 1]),
                (SetOpKind::Union, true, [3, 1, 3]),
                (SetOpKind::Intersect, false, [1, 0, 1]),
                (SetOpKind::Intersect, true, [1, 0, 1]),
                (SetOpKind::Except, false, [0, 1, 0]),
                (SetOpKind::Except, true, [1, 1, 0]),
            ] {
                let scan = |oids: &[Option<u32>]| {
                    Box::new(TableScan::from_rows(
                        vec!["v".into()],
                        oids.iter()
                            .map(|oid| ResultRow::from([("v".into(), value(*oid, shape))]))
                            .collect(),
                    ))
                };
                let mut set = ExternalSetOperation::new_with_evaluator(
                    scan(&[Some(1), Some(1), Some(3), None]),
                    scan(&[Some(1), None, None]),
                    kind,
                    all,
                    budget,
                    Arc::new(Catalog),
                )
                .unwrap();
                let (_, rows) = run_to_rows(&mut set).unwrap();
                let mut counts = [0; 3];
                for row in rows {
                    let index = [
                        value(Some(1), shape),
                        value(Some(3), shape),
                        value(None, shape),
                    ]
                    .iter()
                    .position(|value| value.has_same_representation(&row["v"]))
                    .expect("set output preserves the original key representation");
                    counts[index] += 1;
                }
                assert_eq!(
                    counts, expected,
                    "{kind:?} all={all}, shape={shape}, budget={budget}"
                );
            }
        }
    }
}
