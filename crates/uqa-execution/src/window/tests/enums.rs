//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{ArrayValue, DatumValue, EnumLabelKey, EnumValue};
use uqa_sql::expr::enums::{EnumLabelCatalog, EnumTypeLabels};

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
        panic!("peer equality must not inspect label order")
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        panic!("peer equality must not invoke enum input")
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        panic!("peer equality needs no label diagnostic")
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

impl uqa_sql::expr::EngineHook for Catalog {
    fn nextval(&self, name: &str) -> Result<i64, SQLError> {
        NoSequences.nextval(name)
    }
    fn currval(&self, name: &str) -> Result<i64, SQLError> {
        NoSequences.currval(name)
    }
    fn setval(&self, name: &str, value: i64, called: bool) -> Result<i64, SQLError> {
        NoSequences.setval(name, value, called)
    }
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        Some(self)
    }
}

#[test]
fn spilled_window_peers_use_enum_identity_without_reading_labels() {
    for budget in [1, 4096] {
        for shape in 0..3 {
            let schema = RowSchema::new(vec!["k".into()]);
            let mut partition = BufferedIndexedSpill::new(schema, budget);
            for value in [
                Value::Enum(
                    EnumValue::new(16_384, EnumLabelKey::initial(1).unwrap().remove(0))
                        .with_label_oid(Some(5)),
                ),
                Value::Datum(DatumValue::new(16_384, 0, 5_u32.to_le_bytes().to_vec())),
                Value::Datum(DatumValue::new(16_384, 0, 7_u32.to_le_bytes().to_vec())),
                Value::Null,
                Value::Null,
            ] {
                let value = match shape {
                    0 => value,
                    1 => Value::Array(ArrayValue::try_new(vec![value]).unwrap()),
                    _ => Value::Record(vec![("e".into(), value)].into()),
                };
                partition
                    .push(&PhysicalRow::from_values(vec![value]))
                    .unwrap();
            }
            let order = [ScalarOrder {
                expr: ScalarExpr::Column("k".into()),
                descending: false,
                nulls: None,
            }];
            let arena = PlanSubqueryArena::new(&[], None);
            let mut rows =
                PartitionRows::new(&mut partition, &order, &[], &Catalog, &arena).unwrap();
            assert!(rows.are_peers(0, 1).unwrap());
            assert!(!rows.are_peers(1, 2).unwrap());
            assert!(rows.are_peers(3, 4).unwrap());
            assert!(!rows.are_peers(2, 3).unwrap());
        }
    }
}
