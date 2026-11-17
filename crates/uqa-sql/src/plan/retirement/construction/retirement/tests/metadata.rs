//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnType, FunctionBinding},
    plan::{
        CommandPlan, CteCyclePlan, CtePlanBody, CteSearchPlan, SourcePlan, UnifiedPlan,
        ViewCheckPlan, ViewRuleInsertPlan, ViewRuleUpdatePlan,
    },
};

fn names(width: usize) -> Vec<String> {
    (0..width)
        .map(|i| format!("column_{i}_{}", "x".repeat(64)))
        .collect()
}

fn binding(width: usize) -> FunctionBinding {
    FunctionBinding {
        object_id: None,
        name: "generate_series".into(),
        argument_types: names(width),
        builtin: true,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    }
}

fn query(kind: usize, width: usize, depth: usize) -> QueryPlan {
    let sql = [
        "SELECT * FROM records AS r",
        "SELECT * FROM (VALUES (1)) AS v(a)",
        "SELECT * FROM generate_series(1, 2) AS f(a)",
        "SELECT * FROM ROWS FROM (generate_series(1, 2)) AS f(a)",
        "SELECT * FROM (SELECT 1) AS s(a)",
        "SELECT * FROM left_table JOIN right_table USING (key)",
        "WITH item(a) AS (VALUES (1)) SELECT * FROM item",
    ][kind];
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    match block.from.as_mut().unwrap() {
        SourcePlan::Table {
            column_aliases,
            bound_columns,
            ..
        } => {
            *column_aliases = names(width);
            *bound_columns = Some(names(width));
        }
        SourcePlan::Values {
            column_aliases,
            internal_column_types,
            ..
        } => {
            *column_aliases = names(width);
            *internal_column_types = (0..width).map(|_| Some(ColumnType::Integer)).collect();
            let mut ty = ColumnType::Named("original_type".into());
            for _ in 0..depth {
                ty = ColumnType::Array(Box::new(ty));
            }
            internal_column_types.push(Some(ty));
        }
        SourcePlan::Function {
            binding: target,
            column_aliases,
            column_types,
            ..
        } => {
            *target = Some(binding(width));
            *column_aliases = names(width);
            *column_types = names(width);
        }
        SourcePlan::FunctionGroup {
            functions,
            column_aliases,
            ..
        } => {
            *column_aliases = names(width);
            let function = &mut functions[0];
            function.binding = Some(binding(width));
            function.column_aliases = names(width);
            function.column_types = names(width);
        }
        SourcePlan::Subquery { column_aliases, .. } => *column_aliases = names(width),
        SourcePlan::Join {
            using,
            column_aliases,
            ..
        } => {
            *column_aliases = names(width);
            using.as_mut().unwrap().columns = names(width);
        }
        SourcePlan::RetainedRows(_) => unreachable!(),
    }
    if let Some(cte) = query.ctes.first_mut() {
        cte.columns = names(width);
        cte.search = Some(CteSearchPlan {
            columns: names(width),
            breadth_first: true,
            sequence_column: "sequence".into(),
        });
        cte.cycle = Some(CteCyclePlan {
            columns: names(width),
            mark_column: "mark".into(),
            mark_value: ScalarExpr::Literal(uqa_core::Value::Bool(true)),
            mark_default: ScalarExpr::Literal(uqa_core::Value::Bool(false)),
            path_column: "path".into(),
        });
    }
    *query
}

fn context(control: &ProductionControl<'_>) -> Context {
    let mut context = Context::new(control).unwrap();
    for _ in 0..4 {
        context = context.child(control).unwrap();
    }
    context
}

fn mutation(kind: usize, width: usize) -> QueryPlan {
    let statement = [
        "INSERT INTO records VALUES (1) RETURNING *",
        "UPDATE records SET a = 1 RETURNING *",
        "DELETE FROM records RETURNING *",
        "MERGE INTO records USING source ON true WHEN MATCHED THEN UPDATE SET a = 1 RETURNING *",
    ][kind];
    let sql = format!("WITH changed AS ({statement}) SELECT * FROM changed");
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    let CtePlanBody::Command(command) = &mut query.ctes[0].body else {
        panic!("command");
    };
    match command.as_mut() {
        CommandPlan::Insert(value) => {
            value.view_rule_relations = names(width);
            value.view_rule_insert_plans = vec![ViewRuleInsertPlan {
                relation: "view".into(),
                supplied_columns: names(width),
                input_columns: names(width),
            }];
        }
        CommandPlan::Update(value) => {
            value.view_rule_relations = names(width);
            value.view_rule_update_plans = vec![ViewRuleUpdatePlan {
                relation: "view".into(),
                assigned_columns: names(width),
                input_columns: names(width),
            }];
        }
        CommandPlan::Delete(value) => value.view_rule_relations = names(width),
        CommandPlan::Merge(value) => {
            value.view_checks = names(width)
                .into_iter()
                .map(|view| ViewCheckPlan {
                    view,
                    predicate: ScalarExpr::Literal(uqa_core::Value::Bool(true)),
                })
                .collect();
        }
        _ => unreachable!(),
    }
    *query
}

fn complete(query: QueryPlan) -> usize {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let sibling = native.reserve(37).unwrap();
    let context = context(&control);
    let value = context.retain(query);
    let mut retirement = context.prepare_retirement(&control).unwrap();
    retirement.query(value.take());
    let mut input = input(retirement, &native, &work, &token, &token);
    let exhausted = native.reserve(native.available()).unwrap();
    let mut steps = 0;
    loop {
        let mut ready = None;
        let allocation = allocation_counter::measure(|| ready = advance(&mut input).unwrap());
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        assert!(-allocation.count_current <= 16, "{allocation:?}");
        steps += 1;
        if ready.is_some() {
            break;
        }
        assert!(steps < 100_000);
    }
    drop((input, context, exhausted));
    assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
    steps
}

#[test]
fn mutation_metadata_retirement_bounds_rule_fields_without_cleanup_allocations() {
    for kind in 0..4 {
        assert!(complete(mutation(kind, 2048)) > 2048, "mutation {kind}");
    }
}

#[test]
fn query_source_metadata_retirement_bounds_wide_names_and_deep_types_per_window() {
    for kind in 0..7 {
        assert!(complete(query(kind, 2048, 4096)) > 2048, "source {kind}");
    }
}

#[test]
fn query_source_metadata_retirement_releases_each_cancelled_or_abandoned_prefix() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    for kind in 0..7 {
        let mut finished = false;
        for prefix in 0..512 {
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&native, &original, &invoking);
                let context = context(&control);
                let value = context.retain(query(kind, 3, 7));
                let mut retirement = context.prepare_retirement(&control).unwrap();
                retirement.query(value.take());
                let mut input = input(retirement, &native, &work, &original, &invoking);
                let exhausted = native.reserve(native.available()).unwrap();
                let mut complete = false;
                for _ in 0..prefix {
                    complete = advance(&mut input).unwrap().is_some();
                    if complete {
                        break;
                    }
                }
                if complete {
                    finished = true;
                } else if cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
                }
                assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
                drop((context, exhausted));
                assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
            }
            if finished {
                break;
            }
        }
        assert!(finished, "source {kind}");
    }
}
