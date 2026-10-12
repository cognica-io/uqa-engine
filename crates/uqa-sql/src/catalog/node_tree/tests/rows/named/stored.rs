//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored syntax keeps original ROW nodes after serialization and descriptor changes.

use super::{
    deparse, expressions, parse, Catalog, ColumnType, Field, Fixture, Routines, RowSchema,
    Statement,
};
use crate::ast::Expr;
use crate::FunctionTypeResolver;

pub(super) fn retain(
    expression: &mut Expr,
    schema: &RowSchema,
    catalog: &Catalog<'_>,
) -> Result<(), crate::SQLError> {
    crate::binding::stored_types::bind_stored_expression_type_identities(
        expression,
        &mut |name| FunctionTypeResolver::resolve_type_name(catalog, name),
    )?;
    let original = crate::plan::ExpressionPlan::lower(expression.clone());
    let mut bound = original.clone();
    crate::type_resolution::store_syntax_operand_coercions(
        &mut bound.scalar,
        schema,
        &[],
        catalog,
    )?;
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&original, &bound)?;
    crate::catalog::stored_ast::bind_stored_expression_sites(expression, &sites)?;
    Ok(())
}

#[test]
fn stored_rows_keep_postgresql_nodes_after_serialization_and_descriptor_changes() {
    let fixture: Fixture = serde_json::from_str(include_str!("../../named_rows.json")).unwrap();
    let initial = Catalog(&fixture.states[0].catalog, true);
    let mut failures = Vec::new();
    for case in &fixture.cases {
        let base = ColumnType::from_sql_name(&case.base).unwrap();
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let Statement::CreateDomain(mut domain) = crate::compile(&format!(
            "CREATE DOMAIN d AS {} CHECK({})",
            case.base, case.check
        ))
        .unwrap()
        .remove(0) else {
            panic!("domain declaration")
        };
        let expression = &mut domain.checks[0].expression;
        if let Err(error) = retain(expression, &schema, &initial) {
            failures.push(format!("{}: binding: {error}", case.check));
            continue;
        }
        let serialized = serde_json::to_string(expression).unwrap();
        let expression: Expr = serde_json::from_str(&serialized).unwrap();
        for state in &fixture.states {
            let catalog = Catalog(&state.catalog, false);
            let context = expressions::ExpressionContext {
                schema: &schema,
                domain_value: Some(&base),
                types: Some(&catalog),
                routines: &Routines,
            };
            let node = match context.check(&expression) {
                Ok(node) => Field::Node(node),
                Err(error) => {
                    failures.push(format!("{} in {}: {error}", case.check, state.name));
                    continue;
                }
            };
            let expected = state
                .observations
                .iter()
                .find(|o| o.name == case.name)
                .unwrap();
            if node != parse(&expected.conbin).unwrap() {
                failures.push(format!(
                    "{} in {}: node {node}; expected {}",
                    case.check, state.name, expected.conbin
                ));
            }
            for (pretty, output) in [(false, &expected.sql), (true, &expected.pretty)] {
                let result = deparse::expression(&node, &catalog, pretty);
                if result.as_ref().ok() != Some(output) {
                    failures.push(format!(
                        "{} in {} (pretty={pretty}): {result:?}; expected {output}",
                        case.check, state.name
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn stored_row_layout_rejects_missing_or_inconsistent_definition_metadata() {
    let fixture: Fixture = serde_json::from_str(include_str!("../../named_rows.json")).unwrap();
    let catalog = Catalog(&fixture.states[0].catalog, true);
    let base = ColumnType::Integer;
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
    let Statement::CreateDomain(mut domain) =
        crate::compile("CREATE DOMAIN d AS integer CHECK (ROW(VALUE, 'x')::node_pair IS NULL)")
            .unwrap()
            .remove(0)
    else {
        panic!("domain declaration")
    };
    let original = &mut domain.checks[0].expression;
    retain(original, &schema, &catalog).unwrap();
    let context = expressions::ExpressionContext {
        schema: &schema,
        domain_value: Some(&base),
        types: Some(&catalog),
        routines: &Routines,
    };
    for case in 0..8 {
        let mut expression = original.clone();
        let mut visited = 0;
        crate::catalog::stored_ast::visit_stored_expression(&mut expression, &mut |node| {
            if let Expr::CompositeRow { binding, .. } = node {
                visited += 1;
                match case {
                    0 => binding.layout = None,
                    1 => binding.argument_types = None,
                    2 => binding.layout.as_mut().unwrap().width = -1,
                    3 => binding.layout.as_mut().unwrap().width = 1,
                    4 => binding.attributes[0] = 0,
                    5 => binding.attributes[1] = 1,
                    6 => binding.attributes.reverse(),
                    7 => binding.argument_types.as_mut().unwrap().clear(),
                    _ => unreachable!(),
                }
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(visited, 1);
        // Predecessor definitions remain readable; unavailable physical history is not guessed.
        let restored: Expr =
            serde_json::from_slice(&serde_json::to_vec(&expression).unwrap()).unwrap();
        let error = context.check(&restored).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"), "case {case}: {error}");
        assert!(
            error.to_string().contains("stored ROW"),
            "case {case}: {error}"
        );
    }
}

#[test]
fn predecessor_constructor_discovery_follows_record_identity_casts() {
    use crate::type_resolution::composite_rows::{
        expression_requires_binding, query_requires_binding, statement_requires_binding,
    };
    let fixture: Fixture = serde_json::from_str(include_str!("../../named_rows.json")).unwrap();
    let catalog = Catalog(&fixture.states[0].catalog, true);
    for (source, expected) in [
        ("ROW(1, 'x')::node_pair", true),
        ("ROW(1, 'x')::record::node_pair", true),
        ("ROW(1, 'x')::record::record::node_pair", true),
        ("ROW(ROW(1, 'x'), true)::node_outer", true),
        ("ROW(1, 'x')::record", false),
        ("'x'::text", false),
    ] {
        let mut statement = crate::compile(&format!("SELECT {source}"))
            .unwrap()
            .remove(0);
        let Statement::Select(query) = &statement else {
            panic!("SELECT")
        };
        assert_eq!(
            expression_requires_binding(&query.projections[0].expr, &catalog).unwrap(),
            expected,
            "expression: {source}"
        );
        assert_eq!(
            statement_requires_binding(&mut statement, &catalog).unwrap(),
            expected,
            "statement: {source}"
        );
        let crate::plan::UnifiedPlan::Query(mut query) = crate::plan::UnifiedPlan::lower(statement)
        else {
            panic!("query plan")
        };
        assert_eq!(
            query_requires_binding(&mut query, &catalog).unwrap(),
            expected,
            "query: {source}"
        );
    }
}
