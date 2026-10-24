//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn arithmetic_results_keep_the_analyzed_type_outside_the_input_row_scope() {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE t(d date, n bigint)")
        .unwrap()
        .remove(0)
    else {
        panic!("table");
    };
    let context = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "t"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        ..crate::binding::fixture::empty_binding_context()
    };
    for (sql, expected) in [
        (
            "SELECT NULL::pg_catalog.interval+d FROM t",
            ColumnType::Timestamp,
        ),
        ("SELECT NULL::integer+n FROM t", ColumnType::BigInteger),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        for _ in 0..2 {
            read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None).unwrap();
            let UnifiedPlan::Query(query) = &plan else {
                panic!("query");
            };
            let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
                panic!("block");
            };
            let ScalarExpr::Collated { ty, collation, .. } = &block.projections[0].expr else {
                panic!("analyzed arithmetic result must survive input pruning");
            };
            assert_eq!(ty, &expected, "{sql}");
            assert_eq!(collation.oid(), 0);
            plan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        }
    }
}

#[test]
fn concatenation_selection_retains_strict_scalar_and_nonstrict_array_identity() {
    let context = crate::binding::fixture::empty_binding_context();
    for (sql, expected_strict) in [
        ("SELECT NULL::text || nextval('s')::text", true),
        (
            "SELECT NULL::integer[] || ARRAY[nextval('s')::integer]",
            false,
        ),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None).unwrap();
        let mut found = 0;
        let mut inspect = |expression: &ScalarExpr| {
            if let ScalarExpr::Func {
                name,
                binding: Some(binding),
                args,
                ..
            } = expression
            {
                if binding
                    .dispatch
                    .is_some_and(crate::ast::FunctionDispatch::is_concatenation)
                {
                    assert_eq!(
                        crate::expr::bound_scalar_function_strictness(
                            name,
                            Some(binding),
                            args.len()
                        ),
                        Some(expected_strict),
                        "{sql}"
                    );
                    found += 1;
                }
            }
        };
        plan.visit_scalar_expressions(&mut |expression| expression.visit(&mut inspect));
        assert_eq!(found, 1);
    }
}
