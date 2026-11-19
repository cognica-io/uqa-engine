//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{visit_stored_runtime_calls, visit_stored_runtime_calls_from, StoredFunctionSite};
use crate::{
    plan::{CommandPlan, RelationalPlan, UnifiedPlan},
    SQLError, ScalarExpr,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

type Calls = Vec<(String, StoredFunctionSite)>;

const ONE: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

fn input(
    plan: Arc<UnifiedPlan>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    calls: &Arc<AtomicUsize>,
) -> ImageDecoder<Calls, SQLError> {
    let original = original.clone();
    let memory_owner = memory.clone();
    let calls = Arc::clone(calls);
    ImageDecoder::new(memory, &CancellationToken::new(), |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&memory_owner, &original, work.cancellation);
        let mut names = Vec::new();
        visit_stored_runtime_calls_from(
            plan.as_ref().into(),
            &control,
            &mut work,
            &mut |name, site| {
                calls.fetch_add(1, Ordering::Relaxed);
                names.push((name.to_owned(), site));
                Ok(())
            },
        )
        .await?;
        Ok(names)
    })
    .unwrap()
}

#[test]
fn wide_and_deep_native_view_pauses_before_late_calls_and_honors_original_cancellation() {
    let mut stored = plan("SELECT late_call(1) FROM source_rows()");
    let UnifiedPlan::Query(query) = &mut stored else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    let late = std::mem::replace(&mut block.projections[0].expr, ScalarExpr::Default);
    block.grouping_sets = vec![Vec::new(); 1024];
    block.having = Some((0..2048).fold(late, |child, _| ScalarExpr::UnaryMinus(Box::new(child))));
    let stored = Arc::new(stored);
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut cancelled = input(stored.clone(), &memory, &original, &calls);
    for _ in 0..1536 {
        assert!(cancelled
            .advance(&mut std::io::empty(), ONE)
            .unwrap()
            .is_none());
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    original.cancel();
    assert_eq!(
        cancelled
            .advance(&mut std::io::empty(), ONE)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
    let mut resumed = input(stored.clone(), &memory, &CancellationToken::new(), &calls);
    for _ in 0..3072 {
        assert!(resumed
            .advance(&mut std::io::empty(), ONE)
            .unwrap()
            .is_none());
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        resumed.read(&mut std::io::empty()).unwrap(),
        [
            ("late_call".into(), StoredFunctionSite::Expression),
            ("source_rows".into(), StoredFunctionSite::From),
        ]
    );
    assert_eq!(memory.used(), 0);
    let UnifiedPlan::Query(mut query) = Arc::into_inner(stored).unwrap() else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    drop(crate::plan::retirement::Pending::new(
        block.having.take().unwrap(),
    ));
}

#[test]
fn native_command_empty_rows_yield_before_returning_and_release_reader_scratch() {
    let mut stored = plan("INSERT INTO sink VALUES (1) RETURNING late_result()");
    let UnifiedPlan::Command(command) = &mut stored else {
        unreachable!()
    };
    let CommandPlan::Insert(insert) = command.as_mut() else {
        unreachable!()
    };
    insert.rows = vec![Vec::new(); 2048];
    let calls = Arc::new(AtomicUsize::new(0));
    let memory = MemoryBudget::new(64 << 10);
    let mut reader = input(Arc::new(stored), &memory, &CancellationToken::new(), &calls);
    for _ in 0..2048 {
        assert!(reader
            .advance(&mut std::io::empty(), ONE)
            .unwrap()
            .is_none());
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        reader.read(&mut std::io::empty()).unwrap(),
        [("late_result".into(), StoredFunctionSite::Expression)]
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_call_order_matches_ordinary_roots_across_queries_and_commands() {
    let statements = [
        "WITH q AS (SELECT cte_fn(1) AS x) SELECT project_fn(q.x), window_fn(q.x) OVER (ORDER BY window_sort(q.x)) FROM q JOIN ROWS FROM (row_a(arg_fn(1)), row_b()) r ON join_fn(q.x) WHERE filter_fn(q.x) ORDER BY order_fn(q.x)",
        "SELECT left_fn(1) UNION ALL SELECT right_fn((SELECT inner_fn(2)))",
        "WITH moved AS (DELETE FROM ONLY public.items WHERE remove_fn(id) RETURNING return_fn(id)) INSERT INTO saved SELECT insert_fn(id) FROM moved RETURNING give_fn(id)",
        "UPDATE sink SET n = assign_fn((SELECT source_fn(n) FROM row_source())) FROM side WHERE test_fn(side.n) RETURNING final_fn(n)",
    ];
    for sql in statements {
        let stored = Arc::new(plan(sql));
        let mut ordinary = Vec::new();
        stored.visit_scalar_expressions(&mut |root| {
            root.visit(&mut |node| match node {
                ScalarExpr::Func { name, binding, .. } => {
                    assert!(binding.is_none());
                    ordinary.push((name.clone(), StoredFunctionSite::Expression));
                }
                ScalarExpr::WindowCall { name, .. } => {
                    ordinary.push((name.clone(), StoredFunctionSite::Window));
                }
                _ => (),
            });
        });
        super::visit_table_function_calls(
            stored.as_ref().into(),
            &ProductionControl::uncontrolled(),
            &mut |name, _| {
                ordinary.push((name.to_owned(), StoredFunctionSite::From));
                Ok(())
            },
        )
        .unwrap();
        let memory = MemoryBudget::new(64 << 10);
        let original = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &original);
        let mut inline = Vec::new();
        visit_stored_runtime_calls(stored.as_ref().into(), &control, &mut |name, site| {
            inline.push((name.to_owned(), site));
            Ok(())
        })
        .unwrap();
        assert_eq!(inline, ordinary, "{sql}");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut reader = input(stored, &memory, &original, &calls);
        let streamed = (0..256)
            .find_map(|_| reader.advance(&mut std::io::empty(), ONE).unwrap())
            .expect("a short query finishes in its bounded work window");
        assert_eq!(streamed, ordinary, "{sql}");
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn deep_native_query_children_retain_only_outstanding_siblings() {
    let UnifiedPlan::Query(leaf) = plan("SELECT deepest()") else {
        unreachable!()
    };
    let UnifiedPlan::Query(mut empty) = plan("VALUES (1)") else {
        unreachable!()
    };
    empty.root = RelationalPlan::Values {
        rows: Vec::new(),
        subqueries: Vec::new(),
    };
    let mut root = *leaf;
    for _ in 0..4096 {
        root = crate::plan::QueryPlan {
            root: RelationalPlan::Values {
                rows: Vec::new(),
                subqueries: vec![root],
            },
            ..(*empty).clone()
        };
    }
    let stored = Arc::new(UnifiedPlan::Query(Box::new(root)));
    let memory = MemoryBudget::new(64 << 10);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut reader = input(stored.clone(), &memory, &CancellationToken::new(), &calls);
    for _ in 0..4096 {
        assert!(reader
            .advance(&mut std::io::empty(), ONE)
            .unwrap()
            .is_none());
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        reader.read(&mut std::io::empty()).unwrap(),
        [("deepest".into(), StoredFunctionSite::Expression)]
    );
    assert_eq!(memory.used(), 0);
    let UnifiedPlan::Query(query) = Arc::into_inner(stored).unwrap() else {
        unreachable!()
    };
    let mut root = *query;
    while let RelationalPlan::Values {
        ref mut subqueries, ..
    } = root.root
    {
        root = subqueries.pop().unwrap();
    }
}
