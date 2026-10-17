//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{support::Values, *};
use crate::catalog::{range_type::input::*, CatalogReadView};
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken, Value,
};
use uqa_sql::{
    catalog::{
        enum_type::{StoredEnum, StoredEnumLabel},
        index::operator_classes,
    },
    expr::{cast_value_with_type_resolution_with_control, EngineHook},
    SQLError,
};

struct Inputs {
    values: Values,
    endpoint_calls: Cell<usize>,
    canonical_calls: Cell<usize>,
    null_canonical: Cell<bool>,
    cancel_canonical: Option<CancellationToken>,
}

fn enum_definition(reference: &serde_json::Value) -> StoredEnum {
    let types = &reference["metadata"]["types"];
    let oid = |name: &str| types[name].as_str().unwrap().parse::<u32>().unwrap();
    let labels = reference["metadata"]["labels"].as_array().unwrap();
    StoredEnum {
        object_id: [19; 16],
        oid: oid("label"),
        array_oid: oid("_label"),
        array_name: "_label".into(),
        identity: RelationIdentity::new("public", "label"),
        owner: RoleIdentity::BOOTSTRAP,
        usage_acl: None,
        labels: uqa_core::EnumLabelKey::initial(labels.len())
            .unwrap()
            .into_iter()
            .zip(labels)
            .enumerate()
            .map(|(i, (key, label))| StoredEnumLabel {
                oid: label["oid"].as_str().unwrap().parse::<u32>().unwrap(),
                key,
                label: label["label"].as_str().unwrap().into(),
                sort_order: (i + 1) as f32,
            })
            .collect(),
    }
}

impl Inputs {
    fn new(reference: &serde_json::Value) -> Self {
        let types = &reference["metadata"]["types"];
        let oid = |name: &str| types[name].as_str().unwrap().parse::<u32>().unwrap();
        let enumeration = enum_definition(reference);
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "positive".into(),
            oid: oid("positive"),
            array_oid: Some(oid("_positive")),
            base: Box::new(ColumnType::Integer),
            collation_oid: None,
        };
        let mut ranges = BTreeMap::new();
        for (i, (name, subtype, class, collation)) in [
            ("r", ColumnType::Integer, "int4_ops", 0),
            ("dr", domain, "int4_ops", 0),
            ("tr", ColumnType::Text, "text_ops", 950),
            ("ur", ColumnType::Text, "text_ops", 963),
            (
                "ar",
                ColumnType::Array(Box::new(ColumnType::Integer)),
                "array_ops",
                0,
            ),
            ("er", enumeration.column_type(), "enum_ops", 0),
            ("cr", ColumnType::Integer, "int4_ops", 0),
        ]
        .into_iter()
        .enumerate()
        {
            let mut range = definition();
            range.oid = oid(name);
            range.array_oid = oid(&format!("_{name}"));
            range.array_name = format!("_{name}");
            range.identity = RelationIdentity::new("public", name);
            range.object_id = [i as u8 + 1; 16];
            range.subtype = subtype;
            range.collation_oid = collation;
            range.subtype_operator_class = operator_classes::builtin_btree_operator_classes()
                .iter()
                .find(|entry| entry.name == class)
                .unwrap()
                .oid;
            range.multirange.oid = oid(&format!("{name}s"));
            range.multirange.array_oid = oid(&format!("_{name}s"));
            range.multirange.array_name = format!("_{name}s");
            range.multirange.identity = RelationIdentity::new("public", format!("{name}s"));
            range.multirange.object_id = [i as u8 + 11; 16];
            range.canonical_oid = (name == "cr").then(|| {
                reference["metadata"]["canonical_oid"]
                    .as_str()
                    .unwrap()
                    .parse::<u32>()
                    .unwrap()
            });
            ranges.insert(range.identity.qualified_name(), range);
        }
        let mut snapshot = crate::catalog::test_support::empty_catalog()
            .snapshot()
            .clone();
        snapshot.definitions.ranges = Arc::new(ranges);
        snapshot.definitions.enums =
            Arc::new(BTreeMap::from([("public.label".into(), enumeration)]));
        Self {
            values: Values(CatalogReadView::new(snapshot)),
            endpoint_calls: Cell::new(0),
            canonical_calls: Cell::new(0),
            null_canonical: Cell::new(false),
            cancel_canonical: None,
        }
    }

    fn context(&self) -> RangeInputContext<'_> {
        RangeInputContext {
            catalog: &self.values.0,
            expressions: self,
            canonical: self,
            states: None,
        }
    }

    fn target(&self, name: &str) -> ColumnType {
        self.resolve_type_name(name).unwrap().unwrap()
    }

    fn output(
        &self,
        value: &Value,
        ty: &ColumnType,
        control: &ProductionControl<'_>,
    ) -> Produced<Value> {
        cast_value_with_type_resolution_with_control(
            value,
            Some(&ty.catalog_name()),
            "text",
            Some(self),
            control,
        )
        .unwrap()
    }
}

impl EngineHook for Inputs {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn enum_labels(&self) -> Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog> {
        Some(&self.values)
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        if let Some(element) = name.strip_suffix("[]") {
            return self
                .resolve_type_name(element)
                .map(|ty| ty.map(|ty| ColumnType::Array(Box::new(ty))));
        }
        for range in self.values.0.ranges() {
            for ty in [
                range.column_type(),
                range.multirange_type(),
                range.subtype.clone(),
            ] {
                if name == ty.catalog_name() || name == ty.display_name() {
                    return Ok(Some(ty));
                }
            }
            if name == range.identity.name {
                return Ok(Some(range.column_type()));
            }
            if name == range.multirange.identity.name {
                return Ok(Some(range.multirange_type()));
            }
        }
        Ok(ColumnType::from_sql_name(name).ok())
    }
    fn resolve_type_oid(&self, oid: u32) -> Result<Option<ColumnType>, String> {
        uqa_sql::expr::SQLValueCatalog::value_type_by_oid(&self.values, oid)
            .map_err(|error| error.to_string())
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        let ColumnType::Domain { .. } = target else {
            return Ok(None);
        };
        assert_eq!(source, None);
        let value = uqa_sql::expr::cast_value_from(value, "integer", None)?;
        self.endpoint_calls.set(self.endpoint_calls.get() + 1);
        if !matches!(value, Value::Int(x) if x > 0) {
            return Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: "value for domain positive violates check constraint \"positive_check\""
                    .into(),
            });
        }
        Ok(Some(value))
    }
    fn read_range_input(
        &self,
        oid: u32,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        read_input(&self.context(), oid, text, control)
    }
}

impl RangeCanonicalInvocation for Inputs {
    fn invoke_canonical(
        &self,
        catalog: &CatalogReadView,
        oid: u32,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        assert!(catalog
            .ranges()
            .any(|range| range.canonical_oid == Some(oid)));
        self.canonical_calls.set(self.canonical_calls.get() + 1);
        if let Some(token) = &self.cancel_canonical {
            token.cancel();
        }
        Ok(control.copy_value(if self.null_canonical.get() {
            &Value::Null
        } else {
            value
        })?)
    }
}

fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!("input.json")).unwrap()
}

#[test]
fn declared_range_input_matches_postgresql_values_effects_and_error_order() {
    let reference = reference();
    let inputs = Inputs::new(&reference);
    let memory = MemoryBudget::new(262_144);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 32);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        inputs.endpoint_calls.set(0);
        inputs.canonical_calls.set(0);
        inputs.null_canonical.set(name == "canonical_null");
        let target = inputs.target(case["type"].as_str().unwrap());
        let input = Value::Str(case["input"].as_str().unwrap().into());
        let result = cast_value_with_type_resolution_with_control(
            &input,
            None,
            &target.catalog_name(),
            Some(&inputs),
            &control,
        );
        if let Some(error) = case.get("error") {
            let actual = result.unwrap_err();
            assert_eq!(
                actual.sqlstate(),
                error["sqlstate"].as_str(),
                "{name}: {actual}"
            );
            assert_eq!(
                actual.to_string(),
                error["message"].as_str().unwrap(),
                "{name}"
            );
            assert_eq!(
                actual.detail().unwrap_or(""),
                error["detail"].as_str().unwrap(),
                "{name}"
            );
            assert_eq!(
                actual.hint().unwrap_or(""),
                error["hint"].as_str().unwrap(),
                "{name}"
            );
        } else {
            let result = result.unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(
                *inputs.output(&result, &target, &control),
                Value::Str(case["output"].as_str().unwrap().into()),
                "{name}"
            );
            assert_eq!(memory.used(), result.reserved_bytes(), "{name}");
        }
        assert_eq!(
            inputs.endpoint_calls.get() as u64,
            case["endpoint_calls"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            inputs.canonical_calls.get() as u64,
            case["canonical_calls"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(memory.used(), 0, "{name}");
    }
}

#[test]
fn declared_range_casts_retain_identity_and_read_array_members_once() {
    let inputs = Inputs::new(&reference());
    let memory = MemoryBudget::new(262_144);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let target = inputs.target("cr");
    let value = cast_value_with_type_resolution_with_control(
        &Value::Str("[1,2]".into()),
        None,
        &target.catalog_name(),
        Some(&inputs),
        &control,
    )
    .unwrap();
    assert_eq!(inputs.canonical_calls.get(), 1);
    let copy = cast_value_with_type_resolution_with_control(
        &value,
        Some(&target.catalog_name()),
        &target.catalog_name(),
        Some(&inputs),
        &control,
    )
    .unwrap();
    assert_eq!(inputs.canonical_calls.get(), 1);
    assert_eq!(
        *inputs.output(&copy, &target, &control),
        Value::Str("[1,2]".into())
    );
    let wrong = inputs.target("r");
    let error = cast_value_with_type_resolution_with_control(
        &value,
        Some(&target.catalog_name()),
        &wrong.catalog_name(),
        Some(&inputs),
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42846"));
    drop(copy);
    drop(value);
    let array = ColumnType::Array(Box::new(target));
    let values = cast_value_with_type_resolution_with_control(
        &Value::Str("{\"[1,2]\",NULL,\"[3,4]\"}".into()),
        None,
        &array.catalog_name(),
        Some(&inputs),
        &control,
    )
    .unwrap();
    assert_eq!(inputs.canonical_calls.get(), 3);
    drop(values);
    assert_eq!(memory.used(), 0);
}

#[test]
fn declared_range_input_retains_one_allowance_and_releases_failed_support_calls() {
    let mut inputs = Inputs::new(&reference());
    let token = CancellationToken::new();
    let tiny = MemoryBudget::new(0);
    let control = ProductionControl::new(&tiny, &token, &token);
    let target = inputs.target("dr");
    let oid = uqa_sql::catalog::type_metadata::pg_type_oid(&target) as u32;
    assert_eq!(
        read_input(&inputs.context(), oid, "[1,2]", &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(inputs.endpoint_calls.get(), 0);
    assert_eq!(tiny.used(), 0);
    let memory = MemoryBudget::new(262_144);
    for which in 0..2 {
        let external = CancellationToken::new();
        let invoking = CancellationToken::new();
        if which == 0 {
            external.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &external, &invoking);
        assert_eq!(
            read_input(&inputs.context(), oid, "[1,2]", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(inputs.endpoint_calls.get(), 0);
    }
    let token = CancellationToken::new();
    inputs.cancel_canonical = Some(token.clone());
    let control = ProductionControl::new(&memory, &token, &token);
    let oid = uqa_sql::catalog::type_metadata::pg_type_oid(&inputs.target("cr")) as u32;
    assert_eq!(
        read_input(&inputs.context(), oid, "[1,2]", &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(inputs.canonical_calls.get(), 1);
    assert_eq!(memory.used(), 0);
}
