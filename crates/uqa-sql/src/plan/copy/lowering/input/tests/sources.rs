//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FromClause, plan::SourcePlan, ColumnType};

fn source(sql: &str) -> FromClause {
    let Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("source query")
    };
    query.from.take().expect("source fixture")
}

fn expected(source: &FromClause) -> SourcePlan {
    let mut statement = crate::compile("SELECT 1").unwrap().remove(0);
    let Statement::Select(query) = &mut statement else {
        unreachable!()
    };
    query.from = Some(source.clone());
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement.clone()) else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(mut block) = query.root else {
        unreachable!()
    };
    let result = block.from.take().unwrap();
    let native = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let controlled = UnifiedPlan::lower_borrowed_for_rewrite(
        &statement,
        &crate::plan::NoRegisteredAggregates,
        &control,
    )
    .unwrap();
    let crate::plan::PlanRef::Query(controlled) = controlled.as_plan_ref() else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &controlled.root else {
        unreachable!()
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::to_value(block.from.as_ref().unwrap()).unwrap()
    );
    result
}

#[test]
fn retained_source_leaves_preserve_ordinary_fields_and_query_order() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for (sql, queries) in [
        ("SELECT * FROM ONLY public.t AS x(a,b)", vec![]),
        ("SELECT * FROM (VALUES ((SELECT 1),2),((SELECT 3),4)) AS x(a,b)", vec![1,3]),
        ("SELECT * FROM generate_series((SELECT 1),(SELECT 3)) WITH ORDINALITY AS g(n,pos)", vec![1,3]),
        ("SELECT * FROM ROWS FROM(generate_series((SELECT 2),(SELECT 4)),unnest(ARRAY[(SELECT 6)])) WITH ORDINALITY AS g(n,a,pos)", vec![2,4,6]),
    ] {
        let source = source(sql);
        let expected = expected(&source);
        let ((value, memory, observed), polls) = complete(&mut input(Arc::new(Source::From(source)), &native, &work, &token, &token));
        assert_eq!(*value, Field::From(expected), "{sql}");
        assert_eq!(&*observed, &queries);
        assert!(polls > 1);
        assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
        drop((memory, observed));
        assert_eq!((native.used(), work.used()), (0,0));
    }
}

#[test]
fn retained_source_metadata_owns_bound_names_types_and_call_identity() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut table = source("SELECT * FROM t");
    let FromClause::Table {
        name,
        qualifier,
        bound_columns,
        ..
    } = &mut table
    else {
        unreachable!()
    };
    *name = "table_é🙂".repeat(4096);
    *qualifier = "qualifier_é🙂".repeat(4096);
    *bound_columns = Some(vec!["column_é🙂".repeat(4096)]);
    let mut values = source("SELECT * FROM (VALUES (1)) AS x(a)");
    let FromClause::Values {
        internal_column_types,
        internal_column_collations,
        ..
    } = &mut values
    else {
        unreachable!()
    };
    *internal_column_types = vec![Some(ColumnType::Array(Box::new(ColumnType::Text))), None];
    *internal_column_collations = vec![Some(100), None];
    let mut function = source("SELECT * FROM generate_series(1,2)");
    let FromClause::Function {
        binding, relations, ..
    } = &mut function
    else {
        unreachable!()
    };
    let mut selected =
        crate::ast::FunctionBinding::undefined_function("bound_é🙂".repeat(4096), "integer");
    selected.argument_types = vec!["type_é🙂".repeat(4096)];
    selected.object_id = Some([7; 16]);
    *binding = Some(selected);
    *relations = Some(crate::ast::OperatorJoinRelations {
        left: "left_é🙂".repeat(4096),
        right: "right_é🙂".repeat(4096),
    });
    for source in [table, values, function] {
        let expected = expected(&source);
        let source = Arc::new(Source::From(source));
        let ((value, memory, queries), polls) =
            complete(&mut input(source.clone(), &native, &work, &token, &token));
        drop(source);
        assert_eq!(*value, Field::From(expected));
        assert!(polls > 1);
        drop((value, memory, queries));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_source_leaves_share_execution_row_identity_without_copying_rows() {
    use crate::{
        ast::{InternalRelationId, RetainedRowSource},
        RowSchema,
    };
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for count in [0, 1, 1_000_000] {
        let relation = InternalRelationId::allocate();
        let schema = RowSchema::with_internal_relation_metadata(
            relation,
            [(Some(ColumnType::Text), Some(950))],
        );
        let rows = RetainedRowSource::new(relation, schema.clone(), count).unwrap();
        let source = Arc::new(Source::From(FromClause::RetainedRows(rows)));
        let ((value, memory, queries), _) =
            complete(&mut input(source.clone(), &native, &work, &token, &token));
        drop(source);
        let Field::From(SourcePlan::RetainedRows(rows)) = &*value else {
            unreachable!()
        };
        assert_eq!(rows.relation(), relation);
        assert_eq!(rows.rows(), count);
        assert!(rows.schema().shares_index(&schema));
        assert!(queries.is_empty());
        assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
        drop((memory, queries));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_source_leaves_bound_empty_and_wide_metadata_work() {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for width in [0, 2048] {
        let mut table = source("SELECT * FROM t");
        let FromClause::Table { bound_columns, .. } = &mut table else {
            unreachable!()
        };
        *bound_columns = Some(vec![String::new(); width]);
        let mut values = source("SELECT * FROM (VALUES (1)) AS x(a)");
        let FromClause::Values {
            internal_column_types,
            internal_column_collations,
            ..
        } = &mut values
        else {
            unreachable!()
        };
        *internal_column_types = vec![None; width];
        *internal_column_collations = vec![None; width];
        let FromClause::FunctionGroup { mut functions, .. } =
            source("SELECT * FROM ROWS FROM(generate_series(1,2),unnest(ARRAY[1]))")
        else {
            unreachable!()
        };
        let function = functions.remove(0);
        let functions = FromClause::FunctionGroup {
            functions: vec![function; width],
            alias: None,
            column_aliases: Vec::new(),
            ordinality: false,
        };
        for source in [table, values, functions] {
            let expected = expected(&source);
            let ((value, memory, queries), polls) = complete(&mut input(
                Arc::new(Source::From(source)),
                &native,
                &work,
                &token,
                &token,
            ));
            assert_eq!(*value, Field::From(expected));
            assert!(polls > width);
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop((memory, queries));
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_source_leaves_retire_each_suspended_prefix_and_denied_owner() {
    let source = Arc::new(Source::From(source("SELECT * FROM ROWS FROM(generate_series((SELECT 2),(SELECT 4)),unnest(ARRAY[(SELECT 6)])) AS g(n,a)")));
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(value);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(
                    advance(&mut reader).err().unwrap().sqlstate(),
                    Some("57014")
                );
                assert_eq!(
                    advance(&mut reader).err().unwrap().sqlstate(),
                    Some("XX001")
                );
            }
            assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
            assert_eq!((native.used(), work.used()), (37, 0));
        }
    }
    drop(sibling);
    for bytes in [0, 1, 128, 512, 1024] {
        let denied = MemoryBudget::new(bytes);
        let error = input(source.clone(), &denied, &work, &token, &token)
            .read(&mut std::io::empty())
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
}

#[test]
fn retained_source_leaves_retire_deep_prefixes_after_metadata_failure() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for sql in [
                "SELECT * FROM (VALUES (1)) AS x(a)",
                "SELECT * FROM generate_series(1,2) AS x(a)",
                "SELECT * FROM ROWS FROM(generate_series(1,2)) AS x(a)",
            ] {
                let mut source = source(sql);
                let mut deep = Expr::Literal(Value::Int(1));
                for _ in 0..8192 {
                    deep = Expr::Not(Box::new(deep));
                }
                match &mut source {
                    FromClause::Values { rows, alias, .. } => {
                        rows[0][0] = deep;
                        *alias = Some("reject_source_alias".into());
                    }
                    FromClause::Function { args, alias, .. } => {
                        args[0] = deep;
                        *alias = Some("reject_source_alias".into());
                    }
                    FromClause::FunctionGroup {
                        functions, alias, ..
                    } => {
                        functions[0].args[0] = deep;
                        *alias = Some("reject_source_alias".into());
                    }
                    _ => unreachable!(),
                }
                let native = MemoryBudget::new(32 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let sibling = native.reserve(37).unwrap();
                let error = input(
                    Arc::new(Source::From(source)),
                    &native,
                    &work,
                    &token,
                    &token,
                )
                .read(&mut std::io::empty())
                .err()
                .unwrap();
                assert_eq!(error.to_string(), "internal error: rejected source alias");
                assert_eq!((native.used(), work.used()), (37, 0));
                drop(sibling);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
