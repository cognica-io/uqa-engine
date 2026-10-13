//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone composite constants retain `PostgreSQL` tuple bytes and output.

use super::catalog::{Catalog, Type};
use super::{deparse, expressions, parse, Field, Routines};
use crate::expr::composites::CompositeConstantSource;
use crate::{RowSchema, Statement};
use serde::Deserialize;
use uqa_core::{DatumValue, Value};

#[derive(Deserialize)]
struct Fixture {
    catalog: Vec<Type>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    input: String,
    check: String,
    conbin: String,
    sql: String,
    pretty: String,
}

#[test]
fn composite_constants_match_postgresql_tuple_bytes_and_retained_sources() {
    let fixture: Fixture = serde_json::from_str(include_str!("composites.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, true);
    let ty = catalog.named("record_constant").unwrap();
    let context = expressions::ExpressionContext {
        schema: &RowSchema::default(),
        domain_value: None,
        types: Some(&catalog),
        routines: &Routines,
    };
    for case in fixture.cases {
        let Statement::CreateDomain(domain) = crate::compile(&format!(
            "CREATE DOMAIN probe AS integer CHECK ({})",
            case.check
        ))
        .unwrap()
        .remove(0) else {
            panic!("domain")
        };
        let node = context.check(&domain.checks[0].expression).unwrap();
        assert_eq!(Field::Node(node.clone()), parse(&case.conbin).unwrap());
        for (pretty, expected) in [(false, case.sql), (true, case.pretty)] {
            assert_eq!(
                deparse::expression(&node.clone().into(), &catalog, pretty).unwrap(),
                expected
            );
        }
        let Field::Node(constant) = node.field("arg").unwrap() else {
            panic!("composite constant")
        };
        let value = crate::expr::cast_value_with_type_resolution(
            &Value::Str(case.input),
            None,
            "record_constant",
            Some(&catalog),
        )
        .unwrap();
        let source =
            CompositeConstantSource::capture(&value, &ty, Some(&catalog), Some(&catalog)).unwrap();
        let retained =
            super::values::constant_with_source(&Value::Null, &ty, Some(&source), None).unwrap();
        assert_eq!(&retained, constant);
        let Field::Datum { bytes, .. } = constant.field("constvalue").unwrap() else {
            panic!("owned composite bytes")
        };
        let oid = u32::try_from(constant.integer("consttype").unwrap()).unwrap();
        let physical = Value::Datum(DatumValue::new(oid, 0, bytes.clone()));
        assert_eq!(
            super::values::constant_with_source(&physical, &ty, None, None).unwrap(),
            retained
        );
    }
}
