//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::range_type::support::{
    bound_equality_key, compare_bounds, RangeComparisonStates,
};
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken, DatumValue, Value,
};
use uqa_sql::{
    catalog::index::operator_classes,
    expr::{enums::EnumTypeLabels, SQLValueCatalog},
    SQLError,
};

struct Values(crate::catalog::CatalogReadView);

impl SQLValueCatalog for Values {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>, SQLError> {
        Ok(self
            .0
            .ranges()
            .find(|range| {
                uqa_sql::catalog::type_metadata::pg_type_oid(&range.subtype) == i64::from(oid)
            })
            .map(|range| range.subtype.clone()))
    }
    fn value_range_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<uqa_sql::expr::RangeValueDescriptor>>, SQLError> {
        self.0.range_value_descriptor(oid)
    }
    fn compare_range_bounds(
        &self,
        oid: u32,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<std::cmp::Ordering, SQLError> {
        compare_bounds(&self.0, self, None, oid, (left, right), control)
    }
    fn range_bound_equality_key(
        &self,
        oid: u32,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<u8>>, SQLError> {
        bound_equality_key(&self.0, self, oid, value, control)
    }
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok(self
            .0
            .enums()
            .find(|definition| definition.oid == oid)
            .map(|definition| {
                Arc::new(EnumTypeLabels {
                    type_oid: oid,
                    labels: definition
                        .labels
                        .iter()
                        .map(|label| uqa_sql::expr::enums::EnumTypeLabel {
                            oid: label.oid,
                            key: label.key.clone(),
                            label: label.label.clone(),
                        })
                        .collect(),
                })
            }))
    }
    fn enum_label_position(&self, oid: u32) -> Result<Option<(u32, usize)>, SQLError> {
        Ok(self.0.enums().find_map(|definition| {
            definition
                .labels
                .iter()
                .position(|label| label.oid == oid)
                .map(|position| (definition.oid, position))
        }))
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, oid: u32) -> Result<Option<String>, SQLError> {
        Ok(self
            .0
            .enums()
            .find(|definition| definition.oid == oid)
            .map(|definition| definition.identity.name.clone()))
    }
    fn has_enum_types(&self) -> bool {
        self.0.enums().next().is_some()
    }
}

fn selected(
    mut definition: StoredRange,
    ty: ColumnType,
    class: &str,
    collation: u32,
) -> StoredRange {
    definition.subtype = ty;
    definition.subtype_operator_class = operator_classes::builtin_btree_operator_classes()
        .iter()
        .find(|entry| entry.name == class)
        .unwrap()
        .oid;
    definition.collation_oid = collation;
    definition
}

fn physical(case: &serde_json::Value, field: &str, ty: &ColumnType) -> Value {
    let bytes = case[field].as_str().unwrap();
    let mut owned = vec![0xff];
    owned.extend(
        (0..bytes.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&bytes[i..i + 2], 16).unwrap()),
    );
    Value::Datum(DatumValue::new(
        uqa_sql::catalog::type_metadata::pg_type_oid(ty) as u32,
        1,
        owned,
    ))
}

#[test]
fn range_definitions_invoke_selected_comparisons_and_keys_for_postgresql_datums() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/expr/binary/btree/postgresql.json"
    )))
    .unwrap();
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let states = RangeComparisonStates::default();
    for case in reference["cases"].as_array().unwrap() {
        let spelling = case["type"].as_str().unwrap();
        let ty = ColumnType::from_sql_name(spelling).unwrap();
        let definition = selected(
            definition(),
            ty.clone(),
            case["operator_class"].as_str().unwrap(),
            950,
        );
        let values = Values(catalog(definition.clone()));
        let logical = ["left", "right"].map(|field| {
            uqa_sql::expr::cast_value(&Value::Str(case[field].as_str().unwrap().into()), spelling)
                .unwrap()
        });
        let physical =
            ["left_datum_hex", "right_datum_hex"].map(|field| physical(case, field, &ty));
        let expected = case["order"].as_i64().unwrap().cmp(&0);
        for oid in [definition.oid, definition.multirange.oid] {
            for left in [&logical[0], &physical[0]] {
                for right in [&logical[1], &physical[1]] {
                    assert_eq!(
                        compare_bounds(
                            &values.0,
                            &values,
                            Some(&states),
                            oid,
                            (left, right),
                            &control
                        )
                        .unwrap(),
                        expected,
                        "{case}"
                    );
                    let left = bound_equality_key(&values.0, &values, oid, left, &control).unwrap();
                    let right =
                        bound_equality_key(&values.0, &values, oid, right, &control).unwrap();
                    assert_eq!(*left == *right, expected.is_eq(), "{case}");
                }
            }
            let logical_key =
                bound_equality_key(&values.0, &values, oid, &logical[0], &control).unwrap();
            let physical_key =
                bound_equality_key(&values.0, &values, oid, &physical[0], &control).unwrap();
            assert_eq!(*logical_key, *physical_key);
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn range_support_keeps_pinned_selection_and_reuses_definition_lookup_without_allocating() {
    let original = definition();
    let first = Values(catalog(original.clone()));
    let replacement = Values(catalog(selected(
        original.clone(),
        ColumnType::Integer,
        "oid_ops",
        0,
    )));
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let left = Value::Int(-1);
    let right = Value::Int(1);
    assert_eq!(
        first
            .compare_range_bounds(original.oid, &left, &right, &control)
            .unwrap(),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        replacement
            .compare_range_bounds(original.oid, &left, &right, &control)
            .unwrap(),
        std::cmp::Ordering::Greater
    );
    let clone = first.0.clone();
    let allocation = allocation_counter::measure(|| {
        for _ in 0..256 {
            let one = first
                .0
                .range_definition_by_oid(original.oid)
                .unwrap()
                .unwrap();
            let multi = clone
                .range_definition_by_oid(original.multirange.oid)
                .unwrap()
                .unwrap();
            assert!(std::ptr::eq(one, multi));
            assert!(clone
                .range_definition_by_oid(original.array_oid)
                .unwrap()
                .is_none());
            assert_eq!(
                first
                    .compare_range_bounds(original.oid, &left, &right, &control)
                    .unwrap(),
                std::cmp::Ordering::Less
            );
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    let empty = crate::catalog::test_support::empty_catalog();
    assert!(empty
        .range_definition_by_oid(original.oid)
        .unwrap()
        .is_none());
    assert!(first
        .compare_range_bounds(original.array_oid, &left, &right, &control)
        .is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn range_support_preserves_selected_unicode_order_and_resource_errors() {
    let unicode = uqa_sql::catalog::collations::builtin_collation_oid("unicode").unwrap() as u32;
    let definition = selected(definition(), ColumnType::Text, "text_ops", unicode);
    let oid = definition.oid;
    let values = Values(catalog(definition));
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    // The shared independent PostgreSQL record/scalar fixture establishes this Unicode pair.
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/expr/binary/btree/native.json"
    )))
    .unwrap();
    let case = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "text_ops_unicode")
        .unwrap();
    let inputs = case["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sql| {
            let uqa_sql::Statement::Select(query) =
                uqa_sql::compile(&format!("SELECT {}", sql.as_str().unwrap()))
                    .unwrap()
                    .remove(0)
            else {
                panic!("scalar")
            };
            uqa_sql::expr::eval(
                &query.projections[0].expr,
                &uqa_sql::expr::EvalContext::new(None, &[]),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    for (i, left) in inputs.iter().enumerate() {
        for (j, right) in inputs.iter().enumerate() {
            let expected = case["orders"][i * inputs.len() + j]
                .as_i64()
                .unwrap()
                .cmp(&0);
            assert_eq!(
                values
                    .compare_range_bounds(oid, left, right, &control)
                    .unwrap(),
                expected
            );
            assert_eq!(
                *values
                    .range_bound_equality_key(oid, left, &control)
                    .unwrap()
                    == *values
                        .range_bound_equality_key(oid, right, &control)
                        .unwrap(),
                expected.is_eq()
            );
        }
    }
    let empty = MemoryBudget::new(0);
    let constrained = ProductionControl::new(&empty, &token, &token);
    assert_eq!(
        values
            .range_bound_equality_key(oid, &inputs[0], &constrained)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    for cancel_external in [false, true] {
        let external = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_external {
            external.cancel();
        } else {
            invoking.cancel();
        }
        let cancelled = ProductionControl::new(&memory, &external, &invoking);
        assert_eq!(
            values
                .compare_range_bounds(oid, &inputs[0], &inputs[1], &cancelled)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            values
                .range_bound_equality_key(oid, &inputs[0], &cancelled)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
    }
    assert_eq!(memory.used(), 0);
    assert_eq!(empty.used(), 0);
}

fn enum_definition(
    reference: &serde_json::Value,
    name: &str,
) -> uqa_sql::catalog::enum_type::StoredEnum {
    let oid: u32 = reference["metadata"]["types"][name]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let labels = reference["metadata"]["labels"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|label| label["type"].as_str().unwrap().parse::<u32>().unwrap() == oid)
        .collect::<Vec<_>>();
    uqa_sql::catalog::enum_type::StoredEnum {
        object_id: [9; 16],
        oid,
        array_oid: oid + 1000,
        array_name: format!("_{name}"),
        identity: RelationIdentity::new("public", name),
        owner: RoleIdentity::BOOTSTRAP,
        usage_acl: None,
        labels: uqa_core::EnumLabelKey::initial(labels.len())
            .unwrap()
            .into_iter()
            .zip(labels)
            .enumerate()
            .map(
                |(index, (key, label))| uqa_sql::catalog::enum_type::StoredEnumLabel {
                    oid: label["oid"].as_str().unwrap().parse().unwrap(),
                    key,
                    label: label["label"].as_str().unwrap().into(),
                    sort_order: (index + 1) as f32,
                },
            )
            .collect(),
    }
}

#[test]
fn range_and_multirange_share_private_enum_support_without_affecting_other_ranges() {
    for domain in [false, true] {
        enum_support_state(domain);
    }
}

struct EnumScenario {
    values: Values,
    first: StoredRange,
    second: StoredRange,
    declared_oid: u32,
    labels: [u32; 5],
    logical_middle: Value,
}

fn enum_scenario(reference: &serde_json::Value, domain: bool) -> EnumScenario {
    let first_enum = enum_definition(reference, "range_cache_first");
    let second_enum = enum_definition(reference, "range_cache_second");
    let middle = first_enum.label_by_text("middle").unwrap().oid;
    let logical_middle = Value::Enum(
        uqa_core::EnumValue::new(
            first_enum.oid,
            first_enum.label_by_text("middle").unwrap().key.clone(),
        )
        .with_label_oid(Some(middle)),
    );
    let last = first_enum.label_by_text("last").unwrap().oid;
    let foreign_first = second_enum.label_by_text("first").unwrap().oid;
    let foreign_middle = second_enum.label_by_text("middle").unwrap().oid;
    let foreign_last = second_enum.label_by_text("last").unwrap().oid;
    let declared_oid = if domain {
        reference["metadata"]["types"]["range_cache_domain"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    } else {
        first_enum.oid
    };
    let ty = if domain {
        ColumnType::Domain {
            schema: "public".into(),
            name: "enum_domain".into(),
            oid: declared_oid,
            array_oid: Some(90_001),
            collation_oid: None,
            base: Box::new(first_enum.column_type()),
        }
    } else {
        first_enum.column_type()
    };
    let first = selected(definition(), ty, "enum_ops", 0);
    let mut second = first.clone();
    second.oid += 100;
    second.array_oid += 100;
    second.multirange.oid += 100;
    second.multirange.array_oid += 100;
    second.identity.name.push_str("_other");
    let mut snapshot = catalog(first.clone()).snapshot().clone();
    Arc::make_mut(&mut snapshot.definitions.ranges)
        .insert(second.identity.qualified_name(), second.clone());
    snapshot.definitions.enums = Arc::new(BTreeMap::from([
        (first_enum.identity.qualified_name(), first_enum),
        (second_enum.identity.qualified_name(), second_enum),
    ]));
    let values = Values(crate::catalog::CatalogReadView::new(snapshot));
    EnumScenario {
        values,
        first,
        second,
        declared_oid,
        labels: [middle, last, foreign_first, foreign_middle, foreign_last],
        logical_middle,
    }
}

fn enum_support_state(domain: bool) {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("support-state.json")).unwrap();
    let EnumScenario {
        values,
        first,
        second,
        declared_oid,
        labels: [middle, last, foreign_first, foreign_middle, foreign_last],
        logical_middle,
    } = enum_scenario(&reference, domain);
    let states = RangeComparisonStates::default();
    let independent = RangeComparisonStates::default();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(65536);
    let control = ProductionControl::new(&memory, &token, &token);
    let datum = |label: u32| {
        Value::Datum(DatumValue::new(
            declared_oid,
            0,
            label.to_le_bytes().to_vec(),
        ))
    };
    let invoke = |range, left, right, states| {
        compare_bounds(
            &values.0,
            &values,
            Some(states),
            range,
            (&datum(left), &datum(right)),
            &control,
        )
    };
    {
        let physical_key = values
            .range_bound_equality_key(first.oid, &datum(middle), &control)
            .unwrap();
        let logical_key = values
            .range_bound_equality_key(first.oid, &logical_middle, &control)
            .unwrap();
        assert_eq!(*physical_key, *logical_key);
    }
    assert_eq!(
        invoke(first.oid, middle, middle, &states).unwrap(),
        std::cmp::Ordering::Equal
    );
    assert_eq!(
        invoke(first.oid, foreign_first, foreign_last, &states).unwrap(),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        invoke(first.oid, foreign_middle, foreign_last, &states).unwrap(),
        std::cmp::Ordering::Less
    );
    // PostgreSQL retains this private support state after the reference transaction rolls back.
    for (case_name, oid) in [
        ("range_after_rollback", first.oid),
        ("multirange_same_state", first.multirange.oid),
    ] {
        let case_name = if domain {
            format!("domain_{case_name}")
        } else {
            case_name.into()
        };
        let expected = &reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == case_name)
            .unwrap()["error"];
        let error = invoke(oid, middle, last, &states).unwrap_err();
        assert_eq!(error.sqlstate(), expected["sqlstate"].as_str());
        assert_eq!(error.to_string(), expected["message"].as_str().unwrap());
    }
    assert_eq!(
        invoke(second.oid, middle, last, &states).unwrap(),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        invoke(first.oid, middle, last, &independent).unwrap(),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        invoke(first.oid, foreign_middle, foreign_last, &states).unwrap(),
        std::cmp::Ordering::Less
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn range_support_rejects_missing_or_incompatible_selections_without_changing_observation() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(65536);
    let control = ProductionControl::new(&memory, &token, &token);
    for fault in 0..3 {
        let mut definition = selected(definition(), ColumnType::Text, "text_ops", 950);
        match fault {
            0 => definition.subtype_operator_class = u32::MAX,
            1 => definition.subtype = ColumnType::Integer,
            _ => definition.collation_oid = u32::MAX,
        }
        let oid = definition.oid;
        let values = Values(catalog(definition));
        assert!(values.value_range_type(oid).unwrap().is_some());
        let value = Value::Str("a".into());
        let order_error = values
            .compare_range_bounds(oid, &value, &value, &control)
            .unwrap_err();
        let key_error = values
            .range_bound_equality_key(oid, &value, &control)
            .unwrap_err();
        assert_eq!(order_error.to_string(), key_error.to_string());
        assert_eq!(memory.used(), 0);
    }
}
