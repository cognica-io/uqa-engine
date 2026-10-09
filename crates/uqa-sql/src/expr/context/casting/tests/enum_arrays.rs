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

#[test]
fn same_type_enum_cast_preserves_unread_physical_identity() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for bytes in [1_u32.to_le_bytes().to_vec(), vec![1]] {
        let input = Value::Datum(uqa_core::DatumValue::new(16_384, 0, bytes));
        for source in [None, Some("color")] {
            let value = cast_value_with_type_resolution_with_control(
                &input,
                source,
                "color",
                Some(&EnumCatalog),
                &control,
            )
            .unwrap();
            assert!(value.has_same_representation(&input));
            assert_eq!(budget.used(), value.reserved_bytes());
            drop(value);
            assert_eq!(budget.used(), 0);
        }
    }
    let different = Value::Datum(uqa_core::DatumValue::new(
        16_400,
        0,
        1_u32.to_le_bytes().to_vec(),
    ));
    assert_eq!(
        cast_value_with_type_resolution_with_control(
            &different,
            None,
            "color",
            Some(&EnumCatalog),
            &control,
        )
        .unwrap_err()
        .sqlstate(),
        Some("42846")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_enum_oids_fill_legacy_carriers_and_preserve_unread_values() {
    let catalog = Some(&EnumCatalog as &dyn EnumLabelCatalog);
    let fresh = crate::expr::enums::enum_value_from_text(catalog, 16_384, "renamed").unwrap();
    let Value::Enum(label) = &fresh else {
        panic!("enum input")
    };
    assert_eq!(label.label_oid(), Some(16_386));
    let legacy = Value::Enum(label.clone().with_label_oid(None));
    let original = Value::Record(vec![
        ("label".into(), legacy.clone()),
        (
            "labels".into(),
            Value::Array(
                uqa_core::ArrayValue::with_lower_bounds(vec![legacy, Value::Null], vec![-2])
                    .unwrap()
                    .with_element_type_oid(Some(16_384)),
            ),
        ),
        (
            "unread".into(),
            Value::Datum(uqa_core::DatumValue::new(1700, 0, Vec::new())),
        ),
    ]);
    let mut source = crate::expr::composites::CompositeConstantSource {
        value: original.clone(),
        descriptors: Vec::new(),
    };
    source.retain_enum_oids(catalog).unwrap();
    assert_eq!(source.value, original);
    assert!(!source.value.has_same_representation(&original));
    assert!(!crate::expr::enums::has_missing_enum_oid(&source.value));
    let Value::Record(fields) = &source.value else {
        panic!("retained record")
    };
    assert!(fields[0].1.has_same_representation(&fresh));
    let ty = EnumCatalog.resolve_type_name("color").unwrap().unwrap();
    assert_eq!(
        crate::expr::composites::datum::encode_bits(&fields[0].1, &ty),
        Some(16_386)
    );
    let retained = source.clone();
    source.retain_enum_oids(None).unwrap();
    assert_eq!(source, retained);
    let restored: crate::expr::composites::CompositeConstantSource =
        serde_json::from_str(&serde_json::to_string(&source).unwrap()).unwrap();
    assert_eq!(restored, source);
    assert!(crate::expr::enums::retain_enum_oids(None, &original).is_err());
}

impl EnumLabelCatalog for EnumCatalog {
    fn enum_value_by_oid(&self, oid: u32) -> Result<Option<EnumValue>> {
        Ok((oid == 16_386).then(|| {
            EnumValue::new(16_384, EnumLabelKey::from_bytes(vec![0x80]).unwrap())
                .with_label_oid(Some(oid))
        }))
    }

    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>> {
        Ok(matches!(oid, 16_384 | 16_400).then(|| {
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
        if name == "domain#16390" {
            return Ok(Some(hidden_enum_domain().column_type()));
        }
        if let Some(element) = name.strip_suffix("[]") {
            return self
                .resolve_type_name(element)
                .map(|ty| ty.map(|ty| ColumnType::Array(Box::new(ty))));
        }
        Ok(if name == "color" || name == "enum#16384" {
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

    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &ColumnType,
    ) -> Result<Option<Value>> {
        crate::assignment::domain::cast_domain_value(self, value, source, target)
    }
}

#[test]
fn physical_enum_reads_resolve_global_oids_and_obey_production_control() {
    use crate::expr::datums::read_with_catalog_and_control as read;
    let datum = uqa_core::DatumValue::new(16_400, 1, vec![0, 2, 64, 0, 0]);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = read(&datum, Some(&EnumCatalog), &control).unwrap();
    let Value::Enum(label) = &*value else {
        panic!("physical enum")
    };
    assert_eq!(label.type_oid(), 16_384);
    assert_eq!(label.label_oid(), Some(16_386));
    assert!(budget.used() > 0);
    drop(value);
    assert_eq!(budget.used(), 0);
    let empty = MemoryBudget::new(0);
    assert_eq!(
        read(
            &datum,
            Some(&EnumCatalog),
            &ProductionControl::new(&empty, &token, &token)
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    for bytes in [vec![], vec![1, 0, 0]] {
        assert_eq!(
            read(
                &uqa_core::DatumValue::new(16_400, 0, bytes),
                Some(&EnumCatalog),
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("XX001")
        );
    }
    let invalid = uqa_core::DatumValue::new(16_400, 0, 1_u32.to_le_bytes().to_vec());
    let error = read(&invalid, Some(&EnumCatalog), &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P03"));
    assert_eq!(error.to_string(), "invalid internal value for enum: 1");
    token.cancel();
    assert_eq!(
        read(&datum, Some(&EnumCatalog), &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn host_result_output_reads_physical_enums_inside_named_and_positional_values() {
    let value = Value::Datum(uqa_core::DatumValue::new(
        16_400,
        0,
        16_386_u32.to_le_bytes().to_vec(),
    ));
    let record = Value::Record(vec![("label".into(), value.clone())]);
    let mut result = crate::SQLResult::from_rows_with_positions(
        vec!["v".into(), "v".into()],
        vec![crate::ResultRow::new()],
        Some(vec![vec![value, record]]),
    );
    crate::result::render_result_enum_labels(Some(&EnumCatalog), &mut result).unwrap();
    assert_eq!(
        result.positional_rows.unwrap(),
        vec![vec![
            Value::Str("renamed".into()),
            Value::Record(vec![("label".into(), Value::Str("renamed".into()))])
        ]]
    );
    assert!(result.rows[0]
        .values()
        .all(|value| !crate::expr::enums::contains_enum_carrier(value)));
}

fn hidden_enum_domain() -> crate::catalog::domain::StoredDomain {
    crate::catalog::domain::StoredDomain {
        object_id: [3; 16],
        oid: 16_390,
        array_oid: Some(16_391),
        identity: uqa_core::RelationIdentity::new("hidden", "checked_color"),
        owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        definition: crate::ast::CreateDomain {
            name: "hidden.checked_color".into(),
            base: ColumnType::Enum(crate::ast::EnumTypeReference {
                schema: "hidden".into(),
                name: "hidden_color".into(),
                oid: 16_384,
                array_oid: 16_385,
            }),
            collation: None,
            default: None,
            not_null: None,
            checks: vec![],
        },
        array_name: None,
        usage_acl: None,
    }
}

impl crate::catalog::domain::DomainCatalog for EnumCatalog {
    fn domain_by_oid(&self, oid: u32) -> Option<crate::catalog::domain::StoredDomain> {
        (oid == 16_390).then(hidden_enum_domain)
    }
}

impl crate::assignment::AssignmentContext for EnumCatalog {
    fn evaluate_domain_check(
        &self,
        _: &crate::ast::Expr,
        _: &crate::ResultRow,
        _: &crate::RowSchema,
    ) -> Result<Value> {
        unreachable!("this domain tests base identity independently of checks")
    }
}

#[test]
fn domain_base_input_uses_enum_identity_outside_the_search_path() {
    let target = hidden_enum_domain().column_type();
    let expected = Value::Enum(EnumValue::new(
        16_384,
        EnumLabelKey::from_bytes(vec![0x80]).unwrap(),
    ));
    assert_eq!(
        crate::assignment::domain::cast_domain_value(
            &EnumCatalog,
            &Value::Str("renamed".into()),
            None,
            &target,
        )
        .unwrap(),
        Some(expected.clone())
    );
    assert_eq!(
        read_catalog_array_input(
            "{renamed}",
            &ColumnType::Array(Box::new(target)),
            &EnumCatalog
        )
        .unwrap(),
        Value::Array(ArrayValue::try_new(vec![expected]).unwrap())
    );
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
    assert_eq!(output.elements().len(), 0);
    assert_eq!(output.dimensions().len(), 0);
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
