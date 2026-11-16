//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ir::{RetainedConstantValue, RetainedScalarLiteral},
    plan::{mutable::QueryChildSlot, retirement::tests::ScalarSource},
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn native_query_compaction_retires_deep_discarded_payloads_before_constructor_leases() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for outcome in 0..4 {
                let native = MemoryBudget::new(128 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&native, &token, &invoking);
                let mut context = Context::new(&control).unwrap();
                for _ in 0..4 {
                    context = context.child(&control).unwrap();
                }
                let retired = Arc::new(AtomicUsize::new(0));
                let minimum = Arc::new(AtomicUsize::new(0));
                let mut leaf = query("SELECT 1");
                let RelationalPlan::QueryBlock(block) = &mut leaf.root else {
                    panic!("block");
                };
                block.projections[0].expr =
                    ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
                        RetainedConstantValue::new(
                            Arc::new(ScalarSource {
                                native: native.clone(),
                                minimum: minimum.clone(),
                                retired: retired.clone(),
                            }),
                            0,
                        ),
                        None,
                        None,
                        None,
                    ));
                let mut pending = context.retain(leaf.copy_for_rewrite(&control).unwrap());
                drop(leaf);
                let template = query("SELECT (SELECT 1)");
                for _ in 0..512 {
                    context = context.child(&control).unwrap();
                    let mut parent = context.retain(template.copy_for_rewrite(&control).unwrap());
                    let mut slot = parent.query_slot(QueryChildSlot::Subquery(0));
                    drop(context.retain(slot.take()));
                    slot.restore(pending.take());
                    pending = parent;
                }
                let mut outer = query("SELECT 99");
                subqueries::query_arena(&mut outer.root).push(query("SELECT 0"));
                context = context.child(&control).unwrap();
                let mut output = context.retain(outer.copy_for_rewrite(&control).unwrap());
                let mut slot = output.query_slot(QueryChildSlot::Subquery(0));
                drop(context.retain(slot.take()));
                slot.restore(pending.take());
                drop(context);
                minimum.store(native.used(), Ordering::Relaxed);
                let before = native.used();
                let mut reader = input(output, &native, &work, &token, &invoking);
                if outcome == 0 {
                    let (mut output, polls) = complete(&mut reader);
                    assert!(polls > 512);
                    assert!(subqueries::query_arena(&mut output.query.root).is_empty());
                    assert_eq!(
                        serde_json::to_value(&**output).unwrap(),
                        serde_json::to_value(query("SELECT 99")).unwrap()
                    );
                    assert!(native.used() < before);
                    assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
                } else {
                    for _ in 0..64 {
                        assert!(advance(&mut reader).unwrap().is_none());
                    }
                    assert_eq!(retired.load(Ordering::Relaxed), 0);
                    if outcome > 1 {
                        if outcome == 2 {
                            token.cancel();
                        } else {
                            invoking.cancel();
                        }
                        assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                    }
                }
                assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
                assert_eq!(retired.load(Ordering::Relaxed), 1);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
