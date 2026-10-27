//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent composite attribute identity, collation and unknown-result references.

use super::catalog::{Catalog, Type};
use super::{deparse, expressions, parse, Field, Routines};
use crate::{ast::Expr, RowSchema};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    catalog: Vec<Type>,
    renamed_catalog: Vec<Type>,
    cases: Vec<serde_json::Value>,
    errors: Vec<serde_json::Value>,
}

fn expression(base: &str, check: &str) -> Expr {
    let sql = format!("CREATE DOMAIN test_domain AS {base} CHECK ({check})");
    let crate::Statement::CreateDomain(mut domain) = crate::compile(&sql).unwrap().remove(0) else {
        panic!("domain declaration");
    };
    domain.checks.remove(0).expression
}

fn encode(case: &serde_json::Value, catalog: &Catalog<'_>) -> Result<Field, crate::SQLError> {
    let base = case["base"].as_str().unwrap();
    let ty = catalog.named(base).unwrap();
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty.clone())]);
    expressions::ExpressionContext {
        schema: &schema,
        domain_value: Some(&ty),
        types: Some(catalog),
        routines: &Routines,
    }
    .check(&expression(base, case["check"].as_str().unwrap()))
    .map(Field::Node)
}

#[test]
fn typed_composite_field_nodes_preserve_identity_through_attribute_rename() {
    let fixture: Fixture = serde_json::from_str(include_str!("fields.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, false);
    let renamed = Catalog(&fixture.renamed_catalog, false);
    let mut failures = Vec::new();
    for case in &fixture.cases {
        let label = format!("{} {}", case["base"], case["check"]);
        let expected = parse(case["conbin"].as_str().unwrap()).unwrap();
        let actual = match encode(case, &catalog) {
            Ok(actual) => actual,
            Err(error) => {
                failures.push(format!("{label}: {error}"));
                continue;
            }
        };
        if actual != expected {
            failures.push(format!("{label}: actual {actual}\nexpected {expected}"));
        }
        assert_eq!(
            expected,
            parse(case["renamed"]["conbin"].as_str().unwrap()).unwrap()
        );
        for (names, output) in [(&catalog, case), (&renamed, &case["renamed"])] {
            for (pretty, key) in [(false, "sql"), (true, "pretty")] {
                let expected = output[key].as_str().unwrap();
                match deparse::expression(&actual, names, pretty) {
                    Ok(sql) if sql == expected => {}
                    result => failures.push(format!(
                        "{label} pretty={pretty}: {result:?}; expected {expected}"
                    )),
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn composite_field_catalog_diagnostics_and_unknown_types_match_postgresql() {
    let fixture: Fixture = serde_json::from_str(include_str!("fields.json")).unwrap();
    let unknowns: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("field_unknowns.json")).unwrap();
    let collations: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("field_collations.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, false);
    let mut failures = Vec::new();
    for case in fixture.errors.iter().chain(&unknowns).chain(&collations) {
        let actual = encode(case, &catalog);
        let error = &case["error"];
        let label = &case["check"];
        if !error.is_null() {
            match actual {
                Err(actual)
                    if actual.sqlstate() == error["sqlstate"].as_str()
                        && actual.to_string() == error["message"].as_str().unwrap()
                        && actual.detail() == error["detail"].as_str()
                        && actual.hint() == error["hint"].as_str() => {}
                result => failures.push(format!("{label}: {result:?}; expected {error}")),
            }
            continue;
        }
        let expected = parse(case["conbin"].as_str().unwrap()).unwrap();
        match actual {
            Ok(actual) => {
                if actual != expected {
                    failures.push(format!("{label}: actual {actual}\nexpected {expected}"));
                }
                for (pretty, key) in [(false, "sql"), (true, "pretty")] {
                    let expected = case[key].as_str().unwrap();
                    match deparse::expression(&actual, &catalog, pretty) {
                        Ok(sql) if sql == expected => {}
                        result => failures.push(format!(
                            "{label} pretty={pretty}: {result:?}; expected {expected}"
                        )),
                    }
                }
            }
            Err(error) => failures.push(format!("{label}: {error}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn retained_composite_field_nodes_keep_original_attribute_identity_after_rename() {
    let fixture: Fixture = serde_json::from_str(include_str!("fields.json")).unwrap();
    let original = Catalog(&fixture.catalog, false);
    let renamed = Catalog(&fixture.renamed_catalog, false);
    let base = original.named("node_inner").unwrap();
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
    for (position, ty) in [(0, "integer"), (1, "varchar(8)")] {
        let case = &fixture.cases[position];
        let mut stored = expression("node_inner", case["check"].as_str().unwrap());
        let mut count = 0;
        crate::catalog::stored_ast::visit_stored_expression(&mut stored, &mut |node| {
            if let Expr::Func {
                binding: Some(binding),
                ..
            } = node
            {
                if binding.dispatch == Some(crate::ast::FunctionDispatch::FieldSelect) {
                    binding.set_composite_field(crate::ast::CompositeFieldBinding {
                        type_oid: u32::try_from(crate::catalog::type_metadata::pg_type_oid(&base))
                            .unwrap(),
                        number: i16::try_from(position + 2).unwrap(),
                        result_type: crate::ColumnType::from_sql_name(ty).unwrap(),
                        collation_oid: Some(if position == 0 { 0 } else { 950 }),
                        dropped: false,
                        changed_type: None,
                    });
                    count += 1;
                }
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        let stored: Expr = serde_json::from_slice(&serde_json::to_vec(&stored).unwrap()).unwrap();
        let node = Field::Node(
            expressions::ExpressionContext {
                schema: &schema,
                domain_value: Some(&base),
                types: Some(&renamed),
                routines: &Routines,
            }
            .check(&stored)
            .unwrap(),
        );
        assert_eq!(node, parse(case["conbin"].as_str().unwrap()).unwrap());
        for (pretty, key) in [(false, "sql"), (true, "pretty")] {
            assert_eq!(
                deparse::expression(&node, &renamed, pretty).unwrap(),
                case["renamed"][key].as_str().unwrap()
            );
        }
    }
}

#[test]
fn composite_field_reconstruction_rejects_missing_attributes_and_invalid_nodes() {
    let fixture: Fixture = serde_json::from_str(include_str!("fields.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, false);
    let Field::Node(parent) = parse(fixture.cases[1]["conbin"].as_str().unwrap()).unwrap() else {
        panic!("NULL test")
    };
    let Field::Node(selected) = parent.field("arg").unwrap() else {
        panic!("field selection")
    };
    for (name, value) in [
        ("fieldnum", 0.into()),
        ("fieldnum", (-1).into()),
        ("fieldnum", 1.into()),
        ("fieldnum", 4.into()),
        ("fieldnum", i64::MAX.into()),
        ("resulttype", 0.into()),
        ("resultcollid", (-1).into()),
        ("arg", Field::Null),
        ("arg", Field::List(vec![])),
    ] {
        let mut node = selected.clone();
        node.fields
            .iter_mut()
            .find(|(field, _)| field == name)
            .unwrap()
            .1 = value;
        for pretty in [false, true] {
            assert!(
                deparse::expression(&node.clone().into(), &catalog, pretty).is_err(),
                "{name}"
            );
        }
    }
}
