//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::{
    expr::{
        composites::{CompositeAttribute, CompositeTypeDescriptor},
        SQLValueCatalog,
    },
    ColumnType, SQLError,
};

struct Catalog(Arc<CompositeTypeDescriptor>);

impl SQLValueCatalog for Catalog {
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        false
    }
    fn enum_type_labels(
        &self,
        _: u32,
    ) -> Result<Option<Arc<uqa_sql::expr::enums::EnumTypeLabels>>, SQLError> {
        Ok(None)
    }

    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == self.0.type_oid).then(|| self.0.clone()))
    }
}

fn containers(text: &str, oid: u32) -> Vec<Value> {
    let row = Value::Row(
        uqa_core::RowValue::typed(
            vec![Value::Str(text.into())],
            vec![uqa_core::RecordFieldType {
                oid,
                type_modifier: -1,
            }],
        )
        .unwrap(),
    );
    vec![
        row.clone(),
        Value::Record(uqa_core::RecordValue::from_parts(
            vec![("value".into(), Value::Str(text.into()))],
            Some(16_384),
        )),
        Value::Array(
            uqa_core::ArrayValue::try_new(vec![Value::Str(text.into())])
                .unwrap()
                .with_element_type_oid(Some(oid)),
        ),
        Value::Array(uqa_core::ArrayValue::try_new(vec![row]).unwrap()),
    ]
}

#[test]
fn range_sql_hash_and_exact_keys_preserve_postgresql_equality_classes() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/expr/range/comparison/postgresql.json"
    )))
    .unwrap();
    let hasher = std::collections::hash_map::RandomState::new();
    for family in reference["families"].as_array().unwrap() {
        let ty = ColumnType::from_sql_name(family["kind"].as_str().unwrap()).unwrap();
        let oid = uqa_sql::catalog::type_metadata::pg_type_oid(&ty) as u32;
        let catalog = Catalog(Arc::new(CompositeTypeDescriptor {
            type_oid: 16_384,
            relation_oid: 16_385,
            dropped: vec![],
            attributes: vec![CompositeAttribute {
                name: "value".into(),
                ty,
                collation_oid: None,
                number: 1,
            }],
        }));
        let values = family["values"].as_array().unwrap();
        for (i, left) in values.iter().enumerate() {
            for (j, right) in values.iter().enumerate() {
                let equal = family["comparisons"][i][j] == 0;
                for (left, right) in containers(left.as_str().unwrap(), oid)
                    .iter()
                    .zip(containers(right.as_str().unwrap(), oid))
                {
                    let encode = |value| {
                        encode_sql_key_with_enum_catalog(
                            std::iter::once(Some(value)),
                            Some(&catalog),
                        )
                        .unwrap()
                    };
                    assert_eq!(
                        encode(left) == encode(&right),
                        equal,
                        "{}: {left:?} versus {right:?}",
                        family["kind"]
                    );
                    if equal {
                        let hash = |value| {
                            hash_canonical_row_with_enum_catalog(
                                &hasher,
                                std::iter::once(Some(value)),
                                Some(&catalog),
                            )
                            .unwrap()
                        };
                        assert_eq!(hash(left), hash(&right));
                    }
                }
            }
        }
    }
}

#[test]
fn range_key_normalization_respects_the_original_allowance_and_cancellation() {
    let value = containers(&format!("[1,{}.5)", "9".repeat(8_192)), 3906).remove(0);
    let control = StorageReadControl::with_limit(4096);
    let mut output = CatalogKeyOutput {
        values: BudgetedOutput::new(&control),
        enums: None,
        sql_hash: true,
    };
    let error = encode_value(&value, &mut output).unwrap_err();
    assert!(
        matches!(error, ExecError::SQL(ref error) if error.sqlstate() == Some("53200")),
        "{error}"
    );
    drop(output);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    let mut output = CatalogKeyOutput {
        values: BudgetedOutput::new(&control),
        enums: None,
        sql_hash: true,
    };
    let error = encode_value(&value, &mut output).unwrap_err();
    assert!(
        matches!(error, ExecError::SQL(ref error) if error.sqlstate() == Some("57014")),
        "{error}"
    );
    drop(output);
    assert_eq!(control.memory().used(), 0);
}
