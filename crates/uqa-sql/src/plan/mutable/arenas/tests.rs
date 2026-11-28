//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    plan::{
        retirement::{
            construction::{Context, Prefix},
            empty_source,
        },
        RelationalPlan, SourcePlan, UnifiedPlan,
    },
    ScalarExpr,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Output = Prefix<MutableQueryPlan>;
type Input = ImageDecoder<Output, SQLError>;

mod retirement;

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}

fn input(
    mut query: Output,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&native, &original, work.cancellation);
        let retirement = query.context().clone();
        query
            .prune_subqueries_from(&control, Some(&retirement), &mut work)
            .await?;
        Ok(query)
    })
    .unwrap()
}

fn copied(query: &QueryPlan, control: &ProductionControl<'_>) -> Output {
    let mut context = Context::new(control).unwrap();
    // The copied fixtures have at most four structural levels; scalar depth uses its own allocation-free retirement.
    for _ in 0..4 {
        context = context.child(control).unwrap();
    }
    context.retain(query.copy_for_rewrite(control).unwrap())
}

fn advance<T>(reader: &mut ImageDecoder<T, SQLError>) -> Result<Option<T>, SQLError> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete<T>(reader: &mut ImageDecoder<T, SQLError>) -> (T, usize) {
    for polls in 1..1_000_000 {
        if let Some(output) = advance(reader).unwrap() {
            return (output, polls);
        }
    }
    panic!("scalar-query compaction stopped progressing")
}

#[test]
fn native_query_compaction_keeps_namespaces_and_paired_constructor_owners() {
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    for sql in [
        "SELECT (SELECT 1) FROM (SELECT (SELECT 9)) q",
        "SELECT sum((SELECT 1)) FILTER (WHERE (SELECT true)) OVER (PARTITION BY (SELECT 2) ORDER BY (SELECT 3) ROWS BETWEEN (SELECT 4) PRECEDING AND CURRENT ROW)",
        "SELECT (SELECT 1) FROM (VALUES ((SELECT 2))) a JOIN LATERAL generate_series(1, (SELECT 3)) b ON (SELECT true)",
        "VALUES ((SELECT 1)), ((SELECT 2))",
        "SELECT (SELECT 1) UNION ALL SELECT (SELECT 2) ORDER BY 1 LIMIT (SELECT 3)",
    ] {
        let mut source = query(sql);
        subqueries::query_arena(&mut source.root).insert(0, query("SELECT 999"));
        subqueries::visit_root(&mut source.root, &mut |node| {
            if let ScalarExpr::ScalarSubquery(id) = node { *id += 1; }
        });
        let before = serde_json::to_value(&source).unwrap();
        let expected = query(sql);
        let mut pending = copied(&source, &control);
        let arena = subqueries::query_arena(&mut pending.query.root);
        let (buffer, capacity) = (arena.as_ptr(), arena.capacity());
        let fields = query_fields(pending.memory.as_mut().unwrap());
        let field_buffer = fields.as_ptr();
        let retained = fields[1..].iter_mut().map(|field| field.memory.as_mut().unwrap().reserved_bytes()).collect::<Vec<_>>();
        let before_bytes = pending.reserved_bytes();
        let (mut output, polls) = complete(&mut input(pending, &native, &work, &token, &token));
        assert!(polls > 1);
        assert_eq!(serde_json::to_value(&**output).unwrap(), serde_json::to_value(&expected).unwrap(), "{sql}");
        assert_eq!(serde_json::to_value(&source).unwrap(), before);
        let arena = subqueries::query_arena(&mut output.query.root);
        assert_eq!((arena.as_ptr(), arena.capacity()), (buffer, capacity));
        let fields = query_fields(output.memory.as_mut().unwrap());
        assert_eq!(fields.as_ptr(), field_buffer);
        assert_eq!(fields.iter_mut().map(|field| field.memory.as_mut().unwrap().reserved_bytes()).collect::<Vec<_>>(), retained);
        assert!(output.reserved_bytes() < before_bytes);
        assert_eq!(work.used(), 0);
        assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn native_query_compaction_retires_each_interruption_and_failed_admission() {
    let mut source = query("SELECT (SELECT 1), (SELECT 2) FROM (VALUES ((SELECT 3))) a");
    subqueries::query_arena(&mut source.root).push(query("SELECT 999"));
    let before = serde_json::to_value(&source).unwrap();
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (output, polls) = complete(&mut input(
        copied(&source, &control),
        &native,
        &work,
        &token,
        &token,
    ));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let mut reader = input(
                copied(&source, &control),
                &native,
                &work,
                &original,
                &invoking,
            );
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancel != 0 {
                if cancel == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
            assert_eq!(
                (native.used(), work.used()),
                (37, 0),
                "prefix={prefix}, cancellation={cancel}"
            );
        }
    }
    let pending = copied(&source, &control);
    let exhausted = native.reserve(native.limit() - native.used()).unwrap();
    let mut reader = input(pending, &native, &work, &token, &token);
    assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("53200"));
    drop(reader);
    assert_eq!(native.used(), exhausted.bytes() + sibling.bytes());
    drop(exhausted);
    assert_eq!((native.used(), work.used()), (37, 0));
    assert_eq!(serde_json::to_value(&source).unwrap(), before);
}

#[test]
fn native_query_compaction_preserves_invalid_ids_without_partial_rewriting() {
    let mut source = query("SELECT (SELECT 1), (SELECT 2)");
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        panic!("block")
    };
    block.projections[1].expr = ScalarExpr::ScalarSubquery(usize::MAX);
    subqueries::query_arena(&mut source.root).push(query("SELECT 999"));
    let before = serde_json::to_value(&source).unwrap();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (output, _) = complete(&mut input(
        copied(&source, &control),
        &native,
        &work,
        &token,
        &token,
    ));
    assert_eq!(serde_json::to_value(&**output).unwrap(), before);
    drop(output);
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn native_query_compaction_visits_empty_containers_through_bounded_windows() {
    const WIDTH: usize = 4096;
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    for kind in 0..5 {
        let mut source = query("SELECT 1 WINDOW w AS ()");
        let RelationalPlan::QueryBlock(block) = &mut source.root else {
            panic!("block")
        };
        match kind {
            0 => block.grouping_sets = vec![Vec::new(); WIDTH],
            1 => block.windows = vec![block.windows[0].clone(); WIDTH],
            2 => {
                let mut from = empty_source();
                let SourcePlan::Values { rows, .. } = &mut from else {
                    unreachable!()
                };
                *rows = vec![Vec::new(); WIDTH];
                block.from = Some(from);
            }
            3 => {
                block.from = Some(SourcePlan::FunctionGroup {
                    functions: (0..WIDTH)
                        .map(|_| crate::plan::TableFunctionPlan {
                            name: "f".into(),
                            binding: None,
                            output_name: String::new(),
                            relations: None,
                            args: Vec::new(),
                            column_aliases: Vec::new(),
                            column_types: Vec::new(),
                        })
                        .collect(),
                    alias: None,
                    column_aliases: Vec::new(),
                    ordinality: false,
                });
            }
            4 => {
                source.root = RelationalPlan::Values {
                    rows: vec![Vec::new(); WIDTH],
                    subqueries: Vec::new(),
                };
            }
            _ => unreachable!(),
        }
        subqueries::query_arena(&mut source.root).push(query("SELECT 999"));
        let (mut output, polls) = complete(&mut input(
            copied(&source, &control),
            &native,
            &work,
            &token,
            &token,
        ));
        assert!(polls >= 2 * WIDTH, "container={kind}, polls={polls}");
        assert!(subqueries::query_arena(&mut output.query.root).is_empty());
        assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn native_query_compaction_bounds_deep_scalar_and_source_polling_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(64 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let mut context = Context::new(&control).unwrap().child(&control).unwrap();
            let mut source = query("SELECT (SELECT 0), (SELECT 1)");
            let RelationalPlan::QueryBlock(block) = &mut source.root else {
                panic!("block")
            };
            block.projections.remove(0);
            for _ in 0..8192 {
                let value = std::mem::replace(&mut block.projections[0].expr, ScalarExpr::Star);
                block.projections[0].expr = ScalarExpr::Not(Box::new(value));
            }
            let mut from = empty_source();
            for _ in 0..2048 {
                context = context.child(&control).unwrap();
                from = SourcePlan::Join {
                    comma: false,
                    left: Box::new(from),
                    right: Box::new(empty_source()),
                    kind: crate::ast::JoinKind::Inner,
                    on: Some(ScalarExpr::ScalarSubquery(1)),
                    using: None,
                    natural: false,
                    alias: None,
                    column_aliases: Vec::new(),
                    lateral: false,
                    strategy: crate::plan::JoinExecutionStrategy::Auto,
                };
            }
            block.from = Some(from);
            let mut pending = context.retain(source);
            drop(context);
            let scratch = native.clone();
            let original = token.clone();
            let mut reader = ImageDecoder::new(&work, &token, |mut reader| async move {
                let mut work = reader.input();
                let control = ProductionControl::new(&scratch, &original, work.cancellation);
                let remap = subqueries::prepare_query_from(&pending, control, &mut work)
                    .await?
                    .expect("one unreachable query");
                let mut retirement = pending.context().prepare_retirement(&control)?;
                subqueries::rewrite_query_from(&mut pending, &remap, &control, &mut work).await?;
                compact(
                    subqueries::query_arena(&mut pending.root),
                    None,
                    &|index| remap.keep(index),
                    Some(&mut retirement),
                    Some(&control),
                    &mut work,
                )
                .await?;
                Ok::<_, SQLError>(pending)
            })
            .unwrap();
            let (output, polls) = complete(&mut reader);
            drop(reader);
            assert!(polls > 2 * (8192 + 2048));
            let RelationalPlan::QueryBlock(block) = &output.root else {
                panic!("block")
            };
            assert_eq!(block.subqueries.len(), 1);
            let mut scalar = &block.projections[0].expr;
            while let ScalarExpr::Not(child) = scalar {
                scalar = child;
            }
            assert_eq!(scalar, &ScalarExpr::ScalarSubquery(0));
            assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn projection_privilege_slots_follow_compaction_in_both_query_owners() {
    let memory = MemoryBudget::new(1 << 20);
    let scratch = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut source = query("SELECT (SELECT 10), (SELECT 20), (SELECT 30)");
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        panic!("query block")
    };
    block.projections.drain(..2);
    block.privilege_subqueries.push(1);
    let pending = copied(&source, &control);
    let (native, _) = complete(&mut input(pending, &memory, &scratch, &token, &token));
    let immediate = source
        .copy_for_rewrite(&control)
        .unwrap()
        .prune_subqueries(&control)
        .unwrap();
    for root in [&immediate.root, &native.root] {
        let RelationalPlan::QueryBlock(block) = root else {
            panic!("query block")
        };
        assert_eq!(block.subqueries.len(), 2);
        assert_eq!(block.privilege_subqueries, [0]);
        assert_eq!(block.projections[0].expr, ScalarExpr::ScalarSubquery(1));
    }
    drop((native, immediate));
    assert_eq!(memory.used(), 0);
    assert_eq!(scratch.used(), 0);
}

#[test]
fn projection_privilege_slots_remain_in_their_source_during_transfer() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut source = query("SELECT (SELECT 10), (SELECT 20)");
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        panic!("query block")
    };
    block.projections.clear();
    block.privilege_subqueries.push(1);
    let mut source = source.copy_for_rewrite(&control).unwrap();
    let mut destination = query("SELECT 30").copy_for_rewrite(&control).unwrap();
    for allowed in [true, false] {
        let scalar = ScalarExpr::ScalarSubquery(0)
            .copy_for_rewrite(&control)
            .unwrap();
        let moved;
        (source, destination, _, moved) = source
            .transfer_expression_to(destination, scalar, &control)
            .unwrap();
        assert_eq!(moved, allowed);
        let RelationalPlan::QueryBlock(block) = &source.root else {
            panic!("query block")
        };
        assert_eq!(block.privilege_subqueries, [0]);
        assert_eq!(block.subqueries.len(), 1);
        let RelationalPlan::QueryBlock(block) = &destination.root else {
            panic!("query block")
        };
        assert_eq!(block.subqueries.len(), 1);
    }
    drop((source, destination));
    assert_eq!(memory.used(), 0);
}
