//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{DatumValue, EnumLabelKey, EnumValue};
use uqa_sql::expr::enums::{EnumLabelCatalog, EnumTypeLabels};

struct Catalog;

impl EnumLabelCatalog for Catalog {
    fn has_enum_types(&self) -> bool {
        true
    }
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, uqa_sql::SQLError> {
        Ok((oid == 16_384).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: Vec::new(),
            })
        }))
    }

    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }

    fn enum_type_name(&self, _: u32) -> Result<Option<String>, uqa_sql::SQLError> {
        panic!("join hashing and equality must not call enum output")
    }
}

impl crate::ExpressionEvaluator for Catalog {
    fn evaluate(
        &self,
        expression: &ScalarExpr,
        row: &dyn uqa_sql::expr::RowLookup,
    ) -> ExecResult<Value> {
        TestEvaluator.evaluate(expression, row)
    }

    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        Some(self)
    }
}

#[test]
fn hash_join_uses_enum_identity_in_direct_encoded_and_spilled_keys() {
    let key = EnumLabelKey::initial(1).unwrap().remove(0);
    let physical = Value::Datum(DatumValue::new(16_384, 0, 5_u32.to_le_bytes().to_vec()));
    let native = Value::Enum(EnumValue::new(16_384, key).with_label_oid(Some(5)));
    for wrap in [false, true] {
        let wrap_value = |value: Value| {
            if wrap {
                Value::Record(vec![("e".into(), value)].into())
            } else {
                value
            }
        };
        for (left_count, right_count, budget, computed) in [
            (2, 1, 1 << 20, false),
            (1, 2, 1 << 20, false),
            (2, 2, 1 << 20, false),
            (2, 2, 1, false),
            (2, 1, 1 << 20, true),
        ] {
            let left = TableScan::from_rows(
                vec!["l.k".into()],
                vec![row(&[("l.k", wrap_value(physical.clone()))]); left_count],
            );
            let right = TableScan::from_rows(
                vec!["r.k".into()],
                vec![row(&[("r.k", wrap_value(native.clone()))]); right_count],
            );
            let expression = |name: &str| {
                let key = ScalarExpr::Column(name.into());
                if computed {
                    ScalarExpr::Case {
                        base: None,
                        when: vec![(ScalarExpr::Literal(Value::Bool(true)), key)],
                        else_branch: None,
                    }
                } else {
                    key
                }
            };
            let mut join = HashJoin::new_with_work_mem(
                Box::new(left),
                Box::new(right),
                JoinKind::Inner,
                vec![expression("l.k")],
                vec![expression("r.k")],
                Arc::new(Catalog),
                row(&[("l.k", Value::Null)]),
                row(&[("r.k", Value::Null)]),
                budget,
            );
            let (_, rows) = run_to_rows(&mut join).unwrap();
            assert_eq!(rows.len(), left_count * right_count);
            if budget == 1 {
                assert!(join.hash_index_has_spilled());
            }
        }
    }
}
