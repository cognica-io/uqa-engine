//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::binding::{analyze_query_plan_schema, fixture::empty_binding_context};
use crate::plan::UnifiedPlan;
use crate::{ColumnType, FunctionTypeResolver, SQLError};

struct Routines;
impl FunctionTypeResolver for Routines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl crate::routines::RoutineResolution for Routines {
    fn resolve_table_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[crate::type_resolution::BuiltinFunctionOverload],
    ) -> Result<Option<crate::type_resolution::ResolvedFunctionOverload>, SQLError> {
        assert!(!explicit_variadic);
        crate::type_resolution::resolve_local_builtin_overload(
            name,
            binding,
            argument_names,
            argument_types,
            builtins,
        )
        .map(Some)
    }
}

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.deferred_outer_rewrites.push(DeferredOuterRewrite {
        outer_columns: ordinary_outer_columns(BTreeMap::from([(
            ColumnIdentity::qualified("target", "n"),
            crate::ast::InternalRelationId::allocate().column(0),
        )])),
        replacements: replacements::Replacements::ordinary(BTreeMap::from([
            (
                ColumnIdentity::unqualified("alias_n"),
                OuterReplacement {
                    expression: ExpressionPlan {
                        scalar: ScalarExpr::qualified_column("target", "n"),
                        subqueries: Vec::new(),
                    },
                    qualifier: "target".into(),
                    memory: None,
                },
            ),
            (
                ColumnIdentity::qualified("target", "alias_n"),
                OuterReplacement {
                    expression: ExpressionPlan {
                        scalar: ScalarExpr::qualified_column("target", "n"),
                        subqueries: Vec::new(),
                    },
                    qualifier: "target".into(),
                    memory: None,
                },
            ),
        ])),
        shadowed_columns: uqa_core::memory::OwnedSet::new(),
        shadowed_qualifiers: uqa_core::memory::OwnedSet::new(),
    });
    *query
}

fn schema(qualifier: &str, column: &str, ty: ColumnType) -> RowSchema {
    RowSchema::with_qualified_types(qualifier, vec![column.into()], vec![Some(ty)])
}

#[test]
fn actual_local_types_precede_renamed_outer_types_across_nested_queries() {
    let outer = schema("target", "n", ColumnType::Integer);
    for sql in [
        "SELECT alias_n FROM local",
        "SELECT (SELECT alias_n) FROM local",
        "SELECT (VALUES (alias_n)) FROM local",
        "SELECT (SELECT alias_n UNION ALL SELECT alias_n LIMIT 1) FROM local",
    ] {
        let plan = query(sql);
        for (column, expected) in [
            ("alias_n", ColumnType::Text),
            ("other", ColumnType::Integer),
        ] {
            let mut context = empty_binding_context();
            context
                .ctes
                .insert("local".into(), schema("local", column, ColumnType::Text));
            let output = analyze_query_plan_schema(&Routines, &plan, &[], &context, Some(&outer))
                .unwrap_or_else(|error| panic!("{sql}, {column}: {error:?}"));
            assert_eq!(output.column_type(0), Some(&expected), "{sql}, {column}");
        }
    }
}

#[test]
fn an_open_descriptor_keeps_the_original_name_and_can_bind_different_invocations() {
    let plan = query("SELECT alias_n FROM local");
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        unreachable!()
    };
    let open = RowSchema::with_open_columns(&RowSchema::default(), Some("local"));
    let unresolved = resolve_outer_rewrites(block, &open);
    assert!(
        matches!(&unresolved.projections[0].expr, ScalarExpr::Column(name) if name == "alias_n")
    );
    assert_eq!(unresolved.deferred_outer_rewrites.len(), 1);
    let local = resolve_outer_rewrites(block, &schema("local", "alias_n", ColumnType::Text));
    assert!(matches!(&local.projections[0].expr, ScalarExpr::Column(name) if name == "alias_n"));
    let outer = resolve_outer_rewrites(block, &schema("local", "other", ColumnType::Text));
    assert!(matches!(
        &outer.projections[0].expr,
        ScalarExpr::InternalColumn(_)
    ));
    assert_eq!(block.deferred_outer_rewrites.len(), 1);
}

#[test]
fn local_ambiguity_and_local_qualifiers_cannot_fall_back_to_the_view() {
    let outer = schema("target", "n", ColumnType::Integer);
    let mut context = empty_binding_context();
    for name in ["lhs", "rhs"] {
        context
            .ctes
            .insert(name.into(), schema(name, "alias_n", ColumnType::Text));
    }
    let error = analyze_query_plan_schema(
        &Routines,
        &query("SELECT alias_n FROM lhs CROSS JOIN rhs"),
        &[],
        &context,
        Some(&outer),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42702"));
    let output = analyze_query_plan_schema(
        &Routines,
        &query("SELECT target.alias_n FROM lhs AS target"),
        &[],
        &context,
        Some(&outer),
    )
    .unwrap();
    assert_eq!(output.column_type(0), Some(&ColumnType::Text));
}

#[test]
fn outer_identity_survives_a_local_relation_with_the_same_qualifier() {
    let outer = schema("target", "n", ColumnType::Integer);
    let mut context = empty_binding_context();
    context
        .ctes
        .insert("local".into(), schema("local", "n", ColumnType::Text));
    for sql in [
        "SELECT alias_n FROM local AS target",
        "SELECT (SELECT alias_n) FROM local AS target",
        "SELECT (VALUES (alias_n)) FROM local AS target",
    ] {
        let output =
            analyze_query_plan_schema(&Routines, &query(sql), &[], &context, Some(&outer)).unwrap();
        assert_eq!(output.column_type(0), Some(&ColumnType::Integer), "{sql}");
    }
}

#[test]
fn pending_outer_rewrites_do_not_escape_into_decorrelated_key_expressions() {
    let context = empty_binding_context();
    let plan = query("SELECT 1 FROM generate_series(1, 2) AS local(n) WHERE n = alias_n");
    let correlation = crate::binding::correlation::CorrelationContext {
        routines: &Routines,
        catalog: context.catalog.as_ref(),
        resolution: &context.resolution,
        scoped_relations: None,
    };
    assert!(
        crate::binding::correlation::decorrelate_exists(correlation, &plan)
            .unwrap()
            .is_none()
    );
}

#[test]
fn source_arguments_bind_before_their_own_output_namespace() {
    let outer = schema("target", "n", ColumnType::Integer);
    let context = empty_binding_context();
    for sql in [
        "SELECT alias_n FROM generate_series(1,alias_n) AS g(alias_n)",
        "SELECT result FROM generate_series(1,target.alias_n) AS target(result)",
        "SELECT result FROM generate_series(1,(SELECT alias_n)) AS g(result)",
        "SELECT result FROM (SELECT alias_n AS result) q",
    ] {
        let output = analyze_query_plan_schema(&Routines, &query(sql), &[], &context, Some(&outer))
            .unwrap_or_else(|error| panic!("{sql}: {error:?}"));
        assert_eq!(output.column_type(0), Some(&ColumnType::Integer), "{sql}");
    }
}

#[test]
fn lateral_peers_shadow_outer_view_columns_without_hiding_non_lateral_ancestors() {
    let outer = schema("target", "n", ColumnType::Integer);
    let mut context = empty_binding_context();
    context
        .ctes
        .insert("local".into(), schema("local", "alias_n", ColumnType::Text));
    for (sql, expected) in [
        (
            "SELECT result FROM local CROSS JOIN LATERAL (SELECT alias_n AS result) q",
            ColumnType::Text,
        ),
        (
            "SELECT result FROM local CROSS JOIN (SELECT alias_n AS result) q",
            ColumnType::Integer,
        ),
    ] {
        let output = analyze_query_plan_schema(&Routines, &query(sql), &[], &context, Some(&outer))
            .unwrap_or_else(|error| panic!("{sql}: {error:?}"));
        assert_eq!(output.column_type(0), Some(&expected), "{sql}");
    }
    let error = analyze_query_plan_schema(
        &Routines,
        &query(
            "SELECT result FROM local CROSS JOIN LATERAL generate_series(1,alias_n) AS g(result)",
        ),
        &[],
        &context,
        Some(&outer),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
}

#[test]
fn source_binding_work_grows_with_inputs_instead_of_join_depth() {
    use crate::ast::FunctionBinding;
    use crate::routines::RoutineResolution;
    use crate::type_resolution::{BuiltinFunctionOverload, ResolvedFunctionOverload};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountedRoutines(AtomicUsize);
    impl FunctionTypeResolver for CountedRoutines {
        fn resolve_function_type(
            &self,
            name: &str,
            binding: Option<&FunctionBinding>,
            names: &[Option<String>],
            types: &[Option<ColumnType>],
            variadic: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            Routines.resolve_function_type(name, binding, names, types, variadic)
        }
    }
    impl RoutineResolution for CountedRoutines {
        fn resolve_table_function_overload_with_builtins(
            &self,
            name: &str,
            binding: Option<&FunctionBinding>,
            names: &[Option<String>],
            types: &[Option<ColumnType>],
            variadic: bool,
            builtins: &[BuiltinFunctionOverload],
        ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Routines.resolve_table_function_overload_with_builtins(
                name, binding, names, types, variadic, builtins,
            )
        }
    }

    let context = empty_binding_context();
    let outer = schema("target", "n", ColumnType::Integer);
    let mut per_input = None;
    for inputs in [1, 8, 32] {
        let sources = (0..inputs)
            .map(|index| format!("generate_series(1,2) AS g{index}"))
            .collect::<Vec<_>>()
            .join(" CROSS JOIN ");
        let plan = query(&format!("SELECT alias_n FROM {sources}"));
        let routines = CountedRoutines(AtomicUsize::new(0));
        let output =
            analyze_query_plan_schema(&routines, &plan, &[], &context, Some(&outer)).unwrap();
        assert_eq!(output.column_type(0), Some(&ColumnType::Integer));
        let calls = routines.0.load(Ordering::Relaxed);
        assert!(calls > 0);
        let limit = inputs * *per_input.get_or_insert(calls);
        assert!(
            calls <= limit,
            "{inputs} inputs: {calls} overload analyses exceed {limit}"
        );
    }
}

#[test]
fn each_replacement_captures_only_its_own_retargeted_qualifier() {
    let mut plan = query("SELECT calc FROM local");
    let RelationalPlan::QueryBlock(block) = &mut plan.root else {
        unreachable!()
    };
    let rewrite = &mut block.deferred_outer_rewrites[0];
    let UnifiedPlan::Query(inner) = UnifiedPlan::lower(
        crate::compile("SELECT incoming.n FROM (VALUES ('local'::text)) incoming(n)")
            .unwrap()
            .remove(0),
    ) else {
        unreachable!()
    };
    rewrite.replacements.ordinary_mut().insert(
        ColumnIdentity::unqualified("calc"),
        OuterReplacement {
            expression: ExpressionPlan {
                scalar: ScalarExpr::ScalarSubquery(0),
                subqueries: vec![*inner],
            },
            qualifier: "target".into(),
            memory: None,
        },
    );
    rewrite.replacements.ordinary_mut().insert(
        ColumnIdentity::unqualified("previous"),
        OuterReplacement {
            expression: ExpressionPlan {
                scalar: ScalarExpr::qualified_column("incoming", "n"),
                subqueries: Vec::new(),
            },
            qualifier: "incoming".into(),
            memory: None,
        },
    );
    rewrite.capture_outer_columns();
    let outer = RowSchema::join(
        &schema("target", "n", ColumnType::Integer),
        &schema("incoming", "n", ColumnType::Integer),
        [],
    );
    let mut context = empty_binding_context();
    context
        .ctes
        .insert("local".into(), schema("local", "n", ColumnType::Integer));
    let output = analyze_query_plan_schema(&Routines, &plan, &[], &context, Some(&outer)).unwrap();
    assert_eq!(output.column_type(0), Some(&ColumnType::Text));
}
