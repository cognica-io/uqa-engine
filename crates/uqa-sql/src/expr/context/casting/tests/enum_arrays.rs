//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::enums::{EnumLabelCatalog, EnumTypeLabel, EnumTypeLabels};
use std::sync::Arc;
use uqa_core::{EnumLabelKey, EnumValue};

struct EnumCatalog;

impl EnumLabelCatalog for EnumCatalog {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>> {
        Ok((oid == 16_384).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: vec![EnumTypeLabel {
                    oid: 16_386,
                    key: EnumLabelKey::from_bytes(vec![0x80]).unwrap(),
                    label: "renamed".into(),
                }],
            })
        }))
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>> {
        Ok(Some("color".into()))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

impl EngineHook for EnumCatalog {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        Some(self)
    }
    fn resolve_type_name(&self, name: &str) -> std::result::Result<Option<ColumnType>, String> {
        if let Some(element) = name.strip_suffix("[]") {
            return self
                .resolve_type_name(element)
                .map(|ty| ty.map(|ty| ColumnType::Array(Box::new(ty))));
        }
        Ok(if name == "color" {
            Some(ColumnType::Enum(crate::ast::EnumTypeReference {
                schema: "public".into(),
                name: name.into(),
                oid: 16_384,
                array_oid: 16_385,
            }))
        } else {
            ColumnType::from_sql_name(name).ok()
        })
    }
}

#[test]
fn enum_array_outputs_read_current_labels_and_keep_nulls_and_bounds() {
    let input = Value::Array(
        ArrayValue::with_lower_bounds(
            vec![
                Value::Enum(EnumValue::new(
                    16_384,
                    EnumLabelKey::from_bytes(vec![0x80]).unwrap(),
                )),
                Value::Null,
            ],
            vec![0],
        )
        .unwrap(),
    );
    let budget = MemoryBudget::new(128 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for target in ["text", "text[]"] {
        let output = cast_value_with_type_resolution_with_control(
            &input,
            Some("color[]"),
            target,
            Some(&EnumCatalog),
            &control,
        )
        .unwrap();
        match &*output {
            Value::Str(text) => assert_eq!(text, "[0:1]={renamed,NULL}"),
            Value::Array(array) => {
                assert_eq!(array.lower_bounds(), [0]);
                assert_eq!(
                    array.elements(),
                    [Value::Str("renamed".into()), Value::Null]
                );
            }
            other => panic!("unexpected enum array output: {other:?}"),
        }
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn empty_enum_array_output_uses_its_declared_element_type() {
    let input = Value::Array(crate::expr::casting::parse_pg_array_literal("{}").unwrap());
    let output =
        cast_value_with_type_resolution(&input, Some("color[]"), "text[]", Some(&EnumCatalog))
            .unwrap();
    let Value::Array(output) = output else {
        panic!("array output");
    };
    assert!(output.elements().is_empty());
    assert!(output.dimensions().is_empty());
}

#[test]
fn enum_array_output_quota_failure_releases_workspaces_and_keeps_the_input() {
    let input = Value::Array(
        ArrayValue::try_new(vec![
            Value::Enum(EnumValue::new(
                16_384,
                EnumLabelKey::from_bytes(vec![0x80]).unwrap()
            ));
            32
        ])
        .unwrap(),
    );
    let original = input.clone();
    let budget = MemoryBudget::new(512);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let retained = control.copy_text("retained result").unwrap();
    let used = budget.used();
    for target in ["text", "text[]"] {
        let error = cast_value_with_type_resolution_with_control(
            &input,
            Some("color[]"),
            target,
            Some(&EnumCatalog),
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), used);
        assert_eq!(input, original);
        assert_eq!(&*retained, "retained result");
    }
}
