//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent catalog references for declared attribute identities and collations.

use super::{Catalog, Routines};
use crate::ast::ColumnDef;
use crate::catalog::node_tree::{deparse, expressions, parse};
use crate::schema::ColumnTypeSchema;
use crate::{SQLError, Statement};

#[derive(serde::Deserialize)]
struct Reference {
    create: String,
    dropped: String,
    attributes: Vec<CatalogColumn>,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct CatalogColumn {
    name: String,
    attribute_number: i16,
    collation_oid: u32,
}

#[derive(serde::Deserialize)]
struct Case {
    check: String,
    conbin: String,
    sql: String,
    pretty: String,
}

struct Names<'a>(&'a [ColumnDef]);

impl deparse::ExpressionNames for Names<'_> {
    fn column(&self, attribute: i64) -> Result<String, SQLError> {
        self.0
            .iter()
            .find(|column| column.attribute_number.map(i64::from) == Some(attribute))
            .map(|column| column.name.clone())
            .ok_or_else(|| SQLError::Internal("unknown captured attribute".into()))
    }

    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        deparse::ExpressionNames::routine(&Routines, oid)
    }

    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        deparse::ExpressionNames::type_name(&Routines, oid, modifier)
    }
}

#[test]
fn declared_attribute_nodes_and_sql_match_postgresql_after_column_drop() {
    let reference: Reference =
        serde_json::from_str(include_str!("../declared_column_nodes.json")).unwrap();
    let Statement::CreateTable(mut table) = crate::compile(&reference.create).unwrap().remove(0)
    else {
        panic!("table declaration");
    };
    table
        .columns
        .retain(|column| column.name != reference.dropped);
    for column in &mut table.columns {
        let metadata = reference
            .attributes
            .iter()
            .find(|attribute| attribute.name == column.name)
            .unwrap();
        column.attribute_number = Some(metadata.attribute_number);
        column.collation_oid = Some(metadata.collation_oid);
    }
    // These are supplied catalog inputs, captured independently from PostgreSQL. The codec must not rebuild their ordinal or selected collation from a type-only projection.
    let columns: Vec<ColumnDef> =
        serde_json::from_str(&serde_json::to_string(&table.columns).unwrap()).unwrap();
    let schema = ColumnTypeSchema::new(&columns);
    let context = expressions::ExpressionContext {
        schema: &schema,
        domain_value: None,
        types: Some(&Catalog),
        routines: &Routines,
    };
    let mut failures = Vec::new();
    for case in reference.cases {
        let Statement::CreateDomain(domain) =
            crate::compile(&format!("CREATE DOMAIN d AS integer CHECK({})", case.check))
                .unwrap()
                .remove(0)
        else {
            panic!("check declaration");
        };
        let actual = match context.check(&domain.checks[0].expression) {
            Ok(node) => node.to_string(),
            Err(error) => {
                failures.push(format!("{}: {error}", case.check));
                continue;
            }
        };
        if actual != case.conbin {
            failures.push(format!(
                "{}:\nexpected {}\nactual   {actual}",
                case.check, case.conbin
            ));
            continue;
        }
        let tree = parse(&actual).unwrap();
        for (pretty, expected) in [(false, case.sql), (true, case.pretty)] {
            assert_eq!(
                deparse::expression(&tree, &Names(&columns), pretty).unwrap(),
                expected,
                "{}",
                case.check
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn declared_references_borrow_the_original_catalog_generation() {
    use expressions::ExpressionSchema;
    let Statement::CreateTable(mut table) =
        crate::compile("CREATE TABLE t (v text)").unwrap().remove(0)
    else {
        panic!("table declaration");
    };
    table.columns[0].attribute_number = Some(7);
    table.columns[0].collation_oid = Some(951);
    let schema = ColumnTypeSchema::new(&table.columns);
    let column = schema.column_reference("v", None).unwrap();
    assert!(std::ptr::eq(column.ty, &raw const table.columns[0].ty));
    assert_eq!(column.attribute_number, 7);
    assert_eq!(column.collation_oid, 951);
    assert!(schema.column_reference("v", Some("unknown")).is_err());
    let original = table.columns[0].clone();
    table.columns.push(original);
    assert!(ColumnTypeSchema::new(&table.columns)
        .column_reference("v", None)
        .is_err());
}
