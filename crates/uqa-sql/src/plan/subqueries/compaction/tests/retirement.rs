//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

fn source(count: usize, invalid: bool) -> Arc<QueryPlan> {
    let mut source = query("SELECT 1");
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        panic!("block")
    };
    block.projections[0].expr = ScalarExpr::Row(
        (0..count)
            .map(ScalarExpr::ScalarSubquery)
            .chain(invalid.then_some(ScalarExpr::ScalarSubquery(usize::MAX)))
            .collect(),
    );
    block.subqueries = (0..count)
        .map(|_| crate::plan::retirement::empty_query())
        .collect();
    Arc::new(source)
}

fn input(
    source: Arc<QueryPlan>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&native, &original, work.cancellation);
        assert!(prepare_query_from(&source, control, &mut work)
            .await?
            .is_none());
        Ok(())
    })
    .unwrap()
}

fn advance(reader: &mut ImageDecoder<(), SQLError>) -> Result<Option<()>, SQLError> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn query_compaction_scratch_retires_one_node_per_window_without_mutation() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let node_bytes = {
        let mut map = Remapping::new(1, control);
        map.expression(&ScalarExpr::ScalarSubquery(0)).unwrap();
        native.used()
    };
    assert!(node_bytes > 0);
    for invalid in [false, true] {
        let source = source(256, invalid);
        let before = serde_json::to_value(source.as_ref()).unwrap();
        let mut reader = input(source.clone(), &native, &work, &token, &token);
        let mut retired = 0;
        loop {
            let used = native.used();
            let ready = advance(&mut reader).unwrap();
            let after = native.used();
            if after < used {
                assert_eq!(used - after, node_bytes);
                retired += 1;
            }
            if ready.is_some() {
                break;
            }
        }
        assert_eq!(retired, 256);
        assert_eq!(serde_json::to_value(source.as_ref()).unwrap(), before);
        drop(reader);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn query_compaction_scratch_retirement_observes_each_cancellation_owner() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(47).unwrap();
    for invalid in [false, true] {
        let source = source(16, invalid);
        let before = serde_json::to_value(source.as_ref()).unwrap();
        for prefix in 0..16 {
            for cancel in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut reader = input(source.clone(), &native, &work, &original, &invoking);
                let mut retired = 0;
                while retired < prefix {
                    let used = native.used();
                    assert!(advance(&mut reader).unwrap().is_none());
                    if native.used() < used {
                        retired += 1;
                    }
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
                assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
                assert_eq!(serde_json::to_value(source.as_ref()).unwrap(), before);
            }
        }
    }
}
