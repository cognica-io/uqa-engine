//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::OwnedExecutablePlan;

fn advance<T: 'static>(reader: &mut ImageDecoder<T, SQLError>, steps: usize) -> Result<Option<T>> {
    reader.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}

fn lower(rows: usize, command: bool, memory: &MemoryBudget) -> RetainedExecutablePlan {
    let values = vec!["(1)"; rows].join(",");
    let prefix = if command {
        "INSERT INTO dst VALUES"
    } else {
        "VALUES"
    };
    let statement = crate::compile(&format!("{prefix} {values}"))
        .unwrap()
        .remove(0);
    let token = CancellationToken::new();
    let original = token.clone();
    let budget = memory.clone();
    let mut reader = ImageDecoder::new(memory, &token, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&budget, &original, input.cancellation);
        UnifiedPlan::lower_executable_from_input(
            &statement,
            &NoRegisteredAggregates,
            &control,
            &mut input,
        )
        .await
    })
    .unwrap();
    loop {
        if let Some(plan) = advance(&mut reader, 1024).unwrap() {
            return plan;
        }
    }
}

fn freeze(
    plan: RetainedExecutablePlan,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<OwnedExecutablePlan, SQLError> {
    let budget = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&budget, &original, input.cancellation);
        plan.0.finish_from(&control, &mut input).await
    })
    .unwrap()
}

#[test]
fn executable_publication_folds_row_constructors_in_ordered_windows() {
    for command in [false, true] {
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut counts = Vec::new();
        for rows in [1, 4096] {
            let fixed = lower(rows, command, &memory)
                .0
                .take()
                .finish(&control)
                .unwrap();
            let mut reader = freeze(lower(rows, command, &memory), &memory, &token, &token);
            let mut calls = 0;
            loop {
                let mut result = None;
                let physical =
                    allocation_counter::measure(|| result = advance(&mut reader, 1).unwrap());
                calls += 1;
                assert!(i128::from(physical.count_total) - i128::from(physical.count_current) <= 8);
                if let Some(plan) = result {
                    assert_eq!(
                        serde_json::to_vec(&*plan).unwrap(),
                        serde_json::to_vec(&*fixed).unwrap()
                    );
                    assert_eq!(plan.reserved_bytes(), fixed.reserved_bytes());
                    break;
                }
            }
            counts.push(calls);
            drop((reader, fixed));
            assert_eq!(memory.used(), 0);
        }
        assert!(counts[1] > counts[0] + 4096);
    }
}

#[test]
fn executable_publication_keeps_root_admission_order_and_cancellation_ownership() {
    for command in [false, true] {
        let memory = MemoryBudget::new(8 << 20);
        for cancel in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = freeze(lower(64, command, &memory), &memory, &original, &invoking);
            assert!(advance(&mut reader, 1).unwrap().is_none());
            if cancel {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert_eq!(
                advance(&mut reader, 1).unwrap_err().sqlstate(),
                Some("57014")
            );
            drop(reader);
            assert_eq!(memory.used(), 0);
        }
        let token = CancellationToken::new();
        let mut reader = freeze(lower(64, command, &memory), &memory, &token, &token);
        let pressure = memory.reserve(memory.available()).unwrap();
        if command {
            assert_eq!(
                advance(&mut reader, 1).unwrap_err().sqlstate(),
                Some("53200")
            );
        } else {
            assert!(advance(&mut reader, 1).unwrap().is_none());
            while advance(&mut reader, 32).unwrap().is_none() {}
        }
        drop((reader, pressure));
        assert_eq!(memory.used(), 0);
    }
}
