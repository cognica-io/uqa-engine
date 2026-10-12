//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently captured coercion nodes and SQL grouping.

use super::{expressions, Routines};
use crate::{ColumnType, RowSchema};

#[test]
fn catalog_coercions_keep_postgresql_grouping_around_operator_inputs() {
    #[derive(serde::Deserialize)]
    struct Fixture {
        catalog: Vec<super::catalog::Type>,
        cases: Vec<Case>,
    }
    #[derive(serde::Deserialize)]
    struct Case {
        check: String,
        conbin: String,
        sql: String,
        pretty: String,
    }
    let fixture: Fixture = serde_json::from_str(include_str!("coercion_grouping.json")).unwrap();
    let catalog = super::catalog::Catalog(&fixture.catalog, true);
    let schema = RowSchema::default();
    let context = expressions::ExpressionContext {
        schema: &schema,
        domain_value: Some(&ColumnType::Text),
        types: Some(&catalog),
        routines: &Routines,
    };
    assert_eq!(fixture.cases.len(), 15);
    for case in fixture.cases {
        let statement = format!("CREATE DOMAIN d AS text CHECK ({})", case.check);
        let crate::Statement::CreateDomain(domain) = crate::compile(&statement).unwrap().remove(0)
        else {
            panic!("domain declaration")
        };
        let actual = super::Field::Node(context.check(&domain.checks[0].expression).unwrap());
        assert_eq!(
            actual,
            super::parse(&case.conbin).unwrap(),
            "{}",
            case.check
        );
        for (pretty, expected) in [(false, case.sql), (true, case.pretty)] {
            assert_eq!(
                super::deparse::expression(&actual, &catalog, pretty).unwrap(),
                expected,
                "{}",
                case.check
            );
        }
    }
}
