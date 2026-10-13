//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently captured native operators, helper calls and analysis diagnostics.

use super::{deparse, expressions, Routines};
use crate::{ast::FunctionBinding, ColumnType, RowSchema, SQLError};

#[derive(serde::Deserialize)]
struct Routine {
    oid: i64,
    name: String,
    args: String,
    result_type: String,
}

struct PatternRoutines(Vec<Routine>);

impl crate::FunctionTypeResolver for PatternRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl PatternRoutines {
    fn captured() -> Self {
        Self(serde_json::from_str(include_str!("pattern_helpers.json")).unwrap())
    }
}

impl expressions::ExpressionRoutines for PatternRoutines {
    fn resolve(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        call: crate::type_resolution::RoutineCallDescriptor<'_>,
    ) -> Result<expressions::RoutineIdentity, SQLError> {
        let arguments = call.argument_types;
        let selected = crate::type_resolution::resolve_fixed_builtin_call(
            name,
            binding,
            &vec![None; arguments.len()],
            arguments,
            false,
            None,
        )?
        .expect("captured built-in routine")
        .selected;
        let routine = self
            .0
            .iter()
            .find(|routine| {
                routine.name == name.strip_prefix("pg_catalog.").unwrap_or(name)
                    && routine.args.split(", ").eq(selected
                        .binding
                        .argument_types
                        .iter()
                        .map(String::as_str))
            })
            .expect("captured PostgreSQL signature");
        Ok(expressions::RoutineIdentity::positional(
            routine.oid,
            routine
                .args
                .split(", ")
                .map(|name| ColumnType::from_sql_name(name).unwrap())
                .collect(),
            ColumnType::from_sql_name(&routine.result_type).unwrap(),
        ))
    }
}

impl deparse::ExpressionNames for PatternRoutines {
    fn column(&self, attribute: i64) -> Result<String, SQLError> {
        Routines.column(attribute)
    }

    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        Routines.type_name(oid, modifier)
    }

    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        Ok(vec![self
            .0
            .iter()
            .find(|routine| routine.oid == oid)
            .expect("captured PostgreSQL routine OID")
            .name
            .clone()])
    }
}

#[test]
fn pattern_catalog_nodes_and_sql_match_postgresql() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("patterns.json")).unwrap();
    let accepted = cases
        .into_iter()
        .filter(|case| case["error"].is_null())
        .collect::<Vec<_>>();
    assert_eq!(accepted.len(), 56);
    super::check_typed_nodes_with(
        &serde_json::to_string(&accepted).unwrap(),
        &PatternRoutines::captured(),
    );
}

#[test]
fn pattern_catalog_diagnostics_match_postgresql() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("patterns.json")).unwrap();
    let rejected = cases
        .into_iter()
        .filter(|case| !case["error"].is_null())
        .collect::<Vec<_>>();
    assert_eq!(rejected.len(), 12);
    let routines = PatternRoutines::captured();
    for case in rejected {
        let base = ColumnType::from_sql_name(case["base"].as_str().unwrap()).unwrap();
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: None,
            routines: &routines,
        };
        let statement = format!(
            "CREATE DOMAIN d AS {} CHECK ({})",
            case["base"].as_str().unwrap(),
            case["check"].as_str().unwrap()
        );
        let crate::Statement::CreateDomain(domain) = crate::compile(&statement).unwrap().remove(0)
        else {
            panic!("domain declaration")
        };
        let error = context.check(&domain.checks[0].expression).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            case["error"]["sqlstate"].as_str(),
            "{statement}"
        );
        assert_eq!(
            error.to_string(),
            case["error"]["message"].as_str().unwrap(),
            "{statement}"
        );
        assert_eq!(
            error.detail(),
            case["error"]["detail"].as_str(),
            "{statement}"
        );
        assert_eq!(error.hint(), case["error"]["hint"].as_str(), "{statement}");
    }
}

#[test]
fn retained_pattern_catalog_nodes_preserve_selected_inputs_and_identity() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("patterns.json")).unwrap();
    let routines = PatternRoutines::captured();
    let mut count = 0;
    for case in cases.iter().filter(|case| case["error"].is_null()) {
        let base = ColumnType::from_sql_name(case["base"].as_str().unwrap()).unwrap();
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let statement = format!(
            "CREATE DOMAIN d AS {} CHECK ({})",
            case["base"].as_str().unwrap(),
            case["check"].as_str().unwrap()
        );
        let crate::Statement::CreateDomain(mut domain) =
            crate::compile(&statement).unwrap().remove(0)
        else {
            panic!("domain declaration")
        };
        let mut expression = domain.checks.remove(0).expression;
        let mut previous = None;
        for _ in 0..2 {
            super::collations::retain_expression(&mut expression, &schema, &routines).unwrap();
            let bytes = serde_json::to_vec(&expression).unwrap();
            if let Some(previous) = previous.replace(bytes.clone()) {
                assert_eq!(bytes, previous, "{statement}");
            }
            expression = serde_json::from_slice(&bytes).unwrap();
            let context = expressions::ExpressionContext {
                schema: &schema,
                domain_value: Some(&base),
                types: Some(&routines),
                routines: &routines,
            };
            let actual = super::Field::Node(context.check(&expression).unwrap());
            assert_eq!(
                actual,
                super::parse(case["conbin"].as_str().unwrap()).unwrap(),
                "{statement}"
            );
            for (pretty, column) in [(false, "sql"), (true, "pretty")] {
                assert_eq!(
                    deparse::expression(&actual, &routines, pretty).unwrap(),
                    case[column].as_str().unwrap(),
                    "{statement}"
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 56);
}
