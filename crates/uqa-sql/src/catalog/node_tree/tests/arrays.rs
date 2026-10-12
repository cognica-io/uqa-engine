//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent catalog array nodes before and after enum and composite changes.

use super::catalog::{Catalog, Type};
use super::{deparse, expressions, parse, Field, Routines};
use crate::ast::Expr;
use crate::expr::composites::CompositeConstantSource;
use crate::expr::SQLValueCatalog;
use crate::{ColumnType, RowSchema};
use serde::Deserialize;
use uqa_core::Value;

#[derive(Deserialize)]
struct Fixture {
    catalog: Vec<Type>,
    changed_catalog: Vec<Type>,
    layout_catalog: Vec<Type>,
    cases: Vec<Case>,
    initial: Vec<Observation>,
    changed: Vec<Observation>,
    layout: Vec<Observation>,
}

#[derive(Deserialize)]
struct Case {
    #[serde(rename = "type")]
    ty: String,
    input: String,
    check: String,
}

#[derive(Deserialize)]
struct Observation {
    name: String,
    conbin: String,
    sql: String,
    pretty: String,
}

fn check(observed: &Field, expected: &Observation, catalog: &Catalog<'_>) {
    assert_eq!(
        *observed,
        parse(&expected.conbin).unwrap(),
        "{} node",
        expected.name
    );
    for (pretty, output) in [(false, &expected.sql), (true, &expected.pretty)] {
        assert_eq!(
            deparse::expression(observed, catalog, pretty).unwrap(),
            *output,
            "{} SQL",
            expected.name
        );
    }
}

fn first_constant(field: &Field) -> Option<&super::Node> {
    match field {
        Field::Node(node) if node.kind == "CONST" => Some(node),
        Field::Node(node) => node
            .fields
            .iter()
            .find_map(|(_, field)| first_constant(field)),
        Field::List(fields) => fields.iter().find_map(first_constant),
        _ => None,
    }
}

#[test]
fn physical_array_constants_copy_owned_bytes_without_descriptors() {
    let fixture: Fixture = serde_json::from_str(include_str!("arrays.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, false);
    for observation in &fixture.initial {
        let expression = parse(&observation.conbin).unwrap();
        let node = first_constant(&expression).unwrap();
        let Field::Datum { bytes, .. } = node.field("constvalue").unwrap() else {
            panic!("physical constant")
        };
        let oid = u32::try_from(node.integer("consttype").unwrap()).unwrap();
        let ty = catalog.value_type_by_oid(oid).unwrap().unwrap();
        let original = Value::Datum(uqa_core::DatumValue::new(oid, 0, bytes.clone()));
        let copied = super::values::constant_with_source(&original, &ty, None, None).unwrap();
        assert_eq!(copied, *node, "{} copied Datum", observation.name);
        let mut malformed = copied;
        let (_, layout) = malformed
            .fields
            .iter_mut()
            .find(|(name, _)| name == "constlen")
            .unwrap();
        *layout = 4.into();
        let error = deparse::expression(&malformed.into(), &catalog, false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
    }
}

#[test]
fn named_array_nodes_retain_physical_identity_across_catalog_changes() {
    let fixture: Fixture = serde_json::from_str(include_str!("arrays.json")).unwrap();
    let initial = Catalog(&fixture.catalog, true);
    let changed = Catalog(&fixture.changed_catalog, false);
    let layout = Catalog(&fixture.layout_catalog, false);
    let base = ColumnType::Integer;
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
    for (index, case) in fixture.cases.iter().enumerate() {
        let name = format!("probe_{index}");
        let before = fixture
            .initial
            .iter()
            .find(|value| value.name == name)
            .unwrap();
        let after = fixture
            .changed
            .iter()
            .find(|value| value.name == name)
            .unwrap();
        let crate::Statement::CreateDomain(mut domain) = crate::compile(&format!(
            "CREATE DOMAIN p AS integer CHECK ({})",
            case.check
        ))
        .unwrap()
        .remove(0) else {
            panic!("domain")
        };
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: Some(&initial),
            routines: &Routines,
        };
        let node = Field::Node(
            context
                .check(&domain.checks[0].expression)
                .unwrap_or_else(|error| panic!("{}: {error}", case.ty)),
        );
        check(&node, before, &initial);
        check(&node, after, &changed);
        let after_layout = fixture
            .layout
            .iter()
            .find(|value| value.name == name)
            .unwrap();
        check(&node, after_layout, &layout);

        let mut ty = initial.named(&case.ty).unwrap();
        while let ColumnType::Domain { base, .. } = ty {
            ty = *base;
        }
        let ty = ty.without_type_modifiers();
        let value = crate::expr::cast_value_with_type_resolution(
            &Value::Str(case.input.clone()),
            Some("unknown"),
            &ty.catalog_name(),
            Some(&initial),
        )
        .unwrap();
        let source =
            CompositeConstantSource::capture(&value, &ty, Some(&initial), Some(&initial)).unwrap();
        let Expr::And(parts) = &mut domain.checks[0].expression else {
            panic!("AND")
        };
        let Expr::IsNull { expr, .. } = &mut parts[1] else {
            panic!("NULL test")
        };
        let Expr::Cast { expr, .. } = expr.as_mut() else {
            panic!("cast")
        };
        // The projected value is deliberately different: only the retained source may supply bytes.
        **expr = Expr::TypedLiteral {
            value: Value::Null,
            ty: ty.catalog_name(),
            collation_oid: None,
            composite_source: Some(Box::new(source)),
        };
        let context = expressions::ExpressionContext {
            types: Some(&changed),
            ..context
        };
        let retained = Field::Node(context.check(&domain.checks[0].expression).unwrap());
        check(&retained, after, &changed);
        let context = expressions::ExpressionContext {
            types: Some(&layout),
            ..context
        };
        let retained = Field::Node(context.check(&domain.checks[0].expression).unwrap());
        check(&retained, after_layout, &layout);
    }
}
