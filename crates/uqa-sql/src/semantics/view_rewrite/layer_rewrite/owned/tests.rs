//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionBinding, plan::UnifiedPlan, ColumnType, FunctionTypeResolver, RowSchema};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

struct Routines;
impl FunctionTypeResolver for Routines {
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
impl crate::routines::RoutineResolution for Routines {}
impl crate::plan::AggregateClassifier for Routines {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

fn query(sql: &str) -> crate::plan::QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}

fn layer(queries: Vec<crate::plan::QueryPlan>) -> AutomaticViewLayer {
    AutomaticViewLayer {
        canonical_name: "public.v".into(),
        source_name: "public.base".into(),
        source_qualifier: "src".into(),
        source_column_map: vec![("logical".into(), "physical".into())],
        source_include_descendants: true,
        source_schema: RowSchema::with_qualified_types(
            "src",
            vec!["logical".into()],
            vec![Some(ColumnType::Integer)],
        ),
        columns: Vec::new(),
        predicate: None,
        subqueries: queries,
        check_option: crate::semantics::view_rewrite::ViewCheckOption::None,
    }
}

fn scope(control: &ProductionControl<'_>) -> Result<ControlledBindingSnapshot, SQLError> {
    crate::binding::snapshot::BindingSnapshot::from(crate::binding::fixture::empty_binding_context())
        .copy_with_control(control)
}

#[test]
fn embedded_view_expressions_keep_positions_physical_names_and_lazy_namespace_errors() {
    let input = layer(Vec::new());
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for expression in [
        ScalarExpr::Column(crate::ast::ReferenceName::from("logical").with_position(Some(17))),
        ScalarExpr::QualifiedColumn {
            qualifier: "src".into(),
            column: crate::ast::ReferenceName::from("logical").with_position(Some(23)),
        },
    ] {
        let output = produce(&Routines, &expression, &input, "target", &control, || {
            panic!("no query needs a namespace")
        })
        .unwrap()
        .finish(&control)
        .unwrap();
        let ScalarExpr::QualifiedColumn { qualifier, column } = &output.scalar else {
            panic!("column")
        };
        assert_eq!(qualifier, "target");
        assert_eq!(column.as_str(), "physical");
        let original = match expression {
            ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } => {
                column.position()
            }
            _ => unreachable!(),
        };
        assert_eq!(column.position(), original);
        assert!(output.subqueries.is_empty());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let error = produce(
        &Routines,
        &ScalarExpr::ScalarSubquery(0),
        &input,
        "target",
        &control,
        || panic!("bounds precede namespace selection"),
    )
    .err()
    .unwrap();
    assert!(error
        .to_string()
        .contains("out-of-bounds scalar subquery slot"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn embedded_view_queries_rename_local_targets_and_preserve_correlated_source_shadowing() {
    let queries = vec![
        query("SELECT target.local_value, src.logical FROM (VALUES (3)) AS target(local_value)"),
        query("SELECT src.logical FROM (VALUES (7)) AS src(logical)"),
        query("WITH q AS (SELECT src.logical) SELECT logical FROM (VALUES (9)) AS nested(logical)"),
        query("SELECT src.logical UNION ALL SELECT src.logical"),
    ];
    let input = layer(queries);
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = produce(
        &Routines,
        &ScalarExpr::ScalarSubquery(0),
        &input,
        "target",
        &control,
        || scope(&control),
    )
    .unwrap()
    .finish(&control)
    .unwrap();
    // Every original arena slot survives, including those not named by this occurrence.
    assert_eq!(output.subqueries.len(), 4);
    let mut actual = Vec::new();
    for query in &output.subqueries {
        let mut columns = Vec::new();
        query.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                if let ScalarExpr::QualifiedColumn { qualifier, column } = node {
                    columns.push(format!("{qualifier}.{column}"));
                } else if let ScalarExpr::Column(column) = node {
                    columns.push(column.to_string());
                }
            });
        });
        actual.push(columns);
    }
    assert_eq!(
        actual,
        [
            vec!["__uqa_view_subquery_local_0.local_value", "target.physical"],
            vec!["src.logical"],
            vec!["target.physical", "logical"],
            vec!["target.physical", "target.physical"],
        ]
    );
    let RelationalPlan::QueryBlock(block) = &output.subqueries[0].root else {
        panic!("block")
    };
    let SourcePlan::Values { alias, .. } = block.from.as_ref().unwrap() else {
        panic!("values")
    };
    assert_eq!(alias.as_deref(), Some("__uqa_view_subquery_local_0"));
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn view_query_alpha_renaming_moves_large_payloads_and_accounts_for_replaced_names() {
    let mut input = query("SELECT target.logical, 'payload' FROM (VALUES (1)) AS target(logical)");
    let RelationalPlan::QueryBlock(block) = &mut input.root else {
        panic!("block")
    };
    block.projections[1].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = input.copy_for_rewrite(&control).unwrap();
    let RelationalPlan::QueryBlock(block) = &input.root else {
        panic!("block")
    };
    let ScalarExpr::Literal(Value::Str(value)) = &block.projections[1].expr else {
        panic!("payload")
    };
    let address = value.as_ptr();
    let before = budget.used();
    let mut output = None;
    let observed = allocation_counter::measure(|| {
        output = Some(renaming::query(input, "target", "replacement", false, &control).unwrap());
    });
    assert!(observed.bytes_total < 4096, "{observed:?}");
    assert_eq!(budget.used() as i64, before as i64 + observed.bytes_current);
    let output = output.unwrap();
    let RelationalPlan::QueryBlock(block) = &output.root else {
        panic!("block")
    };
    let ScalarExpr::Literal(Value::Str(value)) = &block.projections[1].expr else {
        panic!("payload")
    };
    assert_eq!(value.as_ptr(), address);
    let output = output.finish(&control).unwrap();
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn view_query_alpha_renaming_preserves_source_forms_and_reference_positions() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (sql, qualifier) in [
        ("SELECT target.id, target.* FROM items AS target", "target"),
        ("SELECT target.id, target.* FROM target", "target"),
        ("SELECT target.id FROM (SELECT 1 AS id) AS target", "target"),
        ("SELECT generate_series.* FROM generate_series(1,3)", "generate_series"),
        ("SELECT target.* FROM ROWS FROM (generate_series(1,3),generate_series(4,6)) AS target", "target"),
        ("SELECT target.id FROM (VALUES(1)) AS target(id) CROSS JOIN (VALUES(2)) AS other(id)", "target"),
        ("SELECT target.* FROM ((VALUES(1)) AS left_input(id) CROSS JOIN (VALUES(2)) AS right_input(n)) AS target", "target"),
    ] {
        let source = query(sql);
        let mut positions = Vec::new();
        source.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                if node.reference_qualifier() == Some(qualifier) {
                    positions.push(node.reference_position());
                }
            });
        });
        assert!(!positions.is_empty(), "{sql}");
        let input = source.copy_for_rewrite(&control).unwrap();
        let output = renaming::query(input, qualifier, "renamed_source", false, &control).unwrap();
        let mut renamed = Vec::new();
        output.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                assert_ne!(node.reference_qualifier(), Some(qualifier), "{sql}");
                if node.reference_qualifier() == Some("renamed_source") {
                    renamed.push(node.reference_position());
                }
            });
        });
        assert_eq!(renamed, positions, "{sql}");
        let RelationalPlan::QueryBlock(block) = &output.root else { panic!("block") };
        let source = block.from.as_ref().unwrap();
        assert!(super::super::source_plan_declares_qualifier(source, "renamed_source"), "{sql}");
        assert!(!super::super::source_plan_declares_qualifier(source, qualifier), "{sql}");
        drop(output);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn view_query_alpha_renaming_releases_partial_name_replacements_and_preserves_siblings() {
    let source = query("SELECT target.id, target.* FROM items AS target");
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = control.copy_text("alive").unwrap();
    let baseline = budget.used();
    let mut failures = 0;
    let mut successes = 0;
    for available in [0, 1, 8, 16, 32, 64, 128, 1024] {
        let input = source.copy_for_rewrite(&control).unwrap();
        let occupied = budget.reserve(budget.available() - available).unwrap();
        match renaming::query(input, "target", "replacement_namespace", false, &control) {
            Ok(output) => {
                successes += 1;
                drop(output);
            }
            Err(error) => {
                failures += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        assert_eq!(
            budget.used(),
            baseline + occupied.bytes(),
            "available={available}"
        );
        drop(occupied);
    }
    assert!(failures > 1 && successes > 0);
    assert_eq!(&**sibling, "alive");
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn view_query_embedding_rejects_large_copies_before_allocation_and_releases_partial_arenas() {
    let mut query = query("SELECT 'payload', src.logical");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    let input = layer(vec![query]);
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = control.copy_text("alive").unwrap();
    let baseline = budget.used();
    let mut failed = 0;
    let mut passed = 0;
    for available in [0, 16, 128, 1024, 16384, (4 << 20) + 512, 5 << 20] {
        let occupied = budget.reserve(budget.available() - available).unwrap();
        let mut result = None;
        let observed = allocation_counter::measure(|| {
            result = Some(produce(
                &Routines,
                &ScalarExpr::ScalarSubquery(0),
                &input,
                "target",
                &control,
                || scope(&control),
            ));
        });
        match result.unwrap() {
            Ok(output) => {
                passed += 1;
                drop(output);
            }
            Err(error) => {
                failed += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        if available <= 16384 {
            assert!(observed.bytes_total < 32768, "{observed:?}");
        }
        assert_eq!(
            budget.used(),
            occupied.bytes() + baseline,
            "available={available}"
        );
        drop(occupied);
    }
    assert!(failed > 1 && passed > 0);
    assert_eq!(&**sibling, "alive");
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn view_query_embedding_preserves_cancellation_and_releases_callback_failures_and_unwind() {
    let input = layer(vec![query("SELECT src.logical")]);
    for statement_cancelled in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        let control = ProductionControl::new(&budget, &statement, &session);
        let sibling = control.copy_text("alive").unwrap();
        let before = budget.used();
        let result = produce(
            &Routines,
            &ScalarExpr::ScalarSubquery(0),
            &input,
            "target",
            &control,
            || {
                let scope = scope(&control)?;
                if statement_cancelled {
                    statement.cancel();
                } else {
                    session.cancel();
                }
                Ok(scope)
            },
        );
        assert_eq!(result.err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(budget.used(), before);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let error = produce(
        &Routines,
        &ScalarExpr::ScalarSubquery(0),
        &input,
        "target",
        &control,
        || Err(SQLError::Internal("namespace failed".into())),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("namespace failed"));
    assert_eq!(budget.used(), 0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = produce(
            &Routines,
            &ScalarExpr::ScalarSubquery(0),
            &input,
            "target",
            &control,
            || panic!("namespace unwinds"),
        );
    }))
    .is_err());
    assert_eq!(budget.used(), 0);
}
