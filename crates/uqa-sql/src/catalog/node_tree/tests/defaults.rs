//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent default assignment nodes, relation context and hidden root coercions.

use super::{deparse, expressions, parse, Field, Routines};
use crate::{schema::ColumnTypeSchema, SQLError, Statement};

struct Names;

impl deparse::ExpressionNames for Names {
    fn column(&self, number: i64) -> Result<String, SQLError> {
        match number {
            1 => Ok("id".into()),
            3 => Ok("txt".into()),
            _ => Err(super::invalid("unexpected fixture column")),
        }
    }
    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        Routines.routine(oid)
    }
    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        Routines.type_name(oid, modifier)
    }
}

#[test]
fn assignment_nodes_and_hidden_root_casts_match_postgresql() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("defaults.json")).unwrap();
    let Statement::CreateTable(table) = crate::compile(fixture["create"].as_str().unwrap())
        .unwrap()
        .remove(0)
    else {
        panic!("fixture table");
    };
    let schema = ColumnTypeSchema::new(&table.columns);
    let encoder = expressions::ExpressionContext {
        schema: &schema,
        domain_value: None,
        types: None,
        routines: &Routines,
    };
    for case in fixture["defaults"].as_array().unwrap() {
        let column = table
            .columns
            .iter()
            .find(|column| column.name == case["column"])
            .unwrap();
        let expression = column
            .default
            .as_ref()
            .or(column
                .generated
                .as_ref()
                .map(|generated| generated.expression.as_ref()))
            .unwrap();
        let node = encoder.assignment(expression, &column.ty).unwrap();
        assert_eq!(
            Field::Node(node),
            parse(case["node"].as_str().unwrap()).unwrap(),
            "{}",
            column.name
        );
    }
    for case in fixture["defaults"]
        .as_array()
        .unwrap()
        .iter()
        .chain(fixture["other"].as_array().unwrap())
    {
        let node = parse(case["node"].as_str().unwrap()).unwrap();
        assert_eq!(parse(&node.to_string()).unwrap(), node);
        for (pretty, key) in [(false, "sql"), (true, "pretty")] {
            assert_eq!(
                deparse::expression(&node, &Names, pretty).unwrap(),
                case[key].as_str().unwrap()
            );
        }
    }
}

#[test]
fn relation_context_validation_precedes_relation_lookup() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("defaults.json")).unwrap();
    let node = parse(fixture["other"][0]["node"].as_str().unwrap()).unwrap();
    deparse::validate_relation(&node, true).unwrap();
    let error = deparse::validate_relation(&node, false).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(error.to_string(), "expression contains variables");
    let other = parse(&node.to_string().replace(":varno 1", ":varno 2")).unwrap();
    let error = deparse::validate_relation(&other, true).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(
        error.to_string(),
        "expression contains variables of more than one relation"
    );
    let query = Field::List(vec![Field::List(
        vec![super::Node::new("QUERY", []).into()],
    )]);
    for relation in [false, true] {
        let error = deparse::validate_relation(&query, relation).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22023"));
        assert_eq!(error.to_string(), "input is a query, not an expression");
    }
}
