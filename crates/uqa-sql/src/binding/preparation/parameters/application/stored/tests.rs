//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    binding::preparation::{
        membership::MembershipAnalysis, ownership::AnalysisVec, tests::NoRoutines,
    },
    plan::{PlanRef, RelationalPlan, UnifiedPlan},
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Application = ImageDecoder<RetainedExecutablePlan, SQLError>;

fn input(
    width: usize,
    selected: bool,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Application {
    let list = std::iter::repeat_n("1", width)
        .collect::<Vec<_>>()
        .join(",");
    let statement = crate::compile(&format!("SELECT (SELECT 2) IN ({list})"))
        .unwrap()
        .remove(0);
    let budget = native.clone();
    let token = original.clone();
    let plan = ImageDecoder::<RetainedExecutablePlan, SQLError>::new(
        work,
        invoking,
        |mut reader| async move {
            let mut channel = reader.input();
            let control = ProductionControl::new(&budget, &token, channel.cancellation);
            UnifiedPlan::lower_executable_from_input(
                &statement,
                &|_: &str| false,
                &control,
                &mut channel,
            )
            .await
        },
    )
    .unwrap()
    .read(&mut std::io::empty())
    .unwrap();
    let control = ProductionControl::new(native, original, invoking);
    let PlanRef::Query(query) = plan.as_plan_ref() else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    let id = AnalysisIdentity::from(&block.projections[0].expr);
    assert_eq!(block.subqueries.len(), 1);
    let mut constants = InputConstants::new(&control);
    // Both runs traverse the identical query and scalar nodes. Only the selected one builds the membership at the last scalar callback.
    constants.6.insert(id, None).unwrap();
    if selected {
        let mut membership = MembershipAnalysis {
            array_items: AnalysisVec::new(&control),
            array_coercions: AnalysisVec::new(&control),
            left_constants: AnalysisVec::new(&control),
        };
        for index in 0..width - 1 {
            membership.array_items.push(index, &control).unwrap();
        }
        for _ in 0..2 {
            membership.left_constants.push(None, &control).unwrap();
        }
        constants.2.insert(id, membership).unwrap();
    }
    let budget = native.clone();
    let token = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&budget, &token, channel.cancellation);
        constants
            .apply_stored_from(plan, &NoRoutines, &[], &control, &mut channel)
            .await
    })
    .unwrap()
}

fn complete(width: usize, selected: bool, steps: usize, after: usize) -> (usize, Option<usize>) {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut job = input(width, selected, &native, &work, &token, &token);
    let mut calls = 0;
    let mut allocation = None;
    let output = loop {
        let previous = native.used();
        let output = job
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
            .unwrap();
        if calls > after && native.used() > previous {
            allocation.get_or_insert(calls);
        }
        calls += 1;
        if let Some(output) = output {
            break output;
        }
        assert!(calls < 1_000_000);
    };
    let PlanRef::Query(query) = output.as_plan_ref() else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    assert_eq!(block.subqueries.len(), 1);
    if selected {
        let ScalarExpr::Or(parts) = &block.projections[0].expr else {
            panic!("expected two comparisons")
        };
        let [ScalarExpr::Func { args, .. }, ScalarExpr::Binary { lhs, .. }] = parts.as_slice()
        else {
            panic!("selected membership")
        };
        assert_eq!(args[0], ScalarExpr::ScalarSubquery(0));
        assert_eq!(**lhs, ScalarExpr::ScalarSubquery(0));
        assert!(matches!(&args[1], ScalarExpr::Array(items) if items.len()+1 == width));
    } else {
        assert!(
            matches!(&block.projections[0].expr, ScalarExpr::InList { list, .. } if list.len() == width)
        );
    }
    drop((output, job));
    assert_eq!((native.used(), work.used()), (0, 0));
    (calls, allocation)
}

#[test]
fn stored_default_input_application_bounds_selected_membership_and_keeps_written_query_slots() {
    const WIDTH: usize = 1024;
    let (traversal, _) = complete(WIDTH, false, 1, usize::MAX);
    let (selected, allocation) = complete(WIDTH, true, 1, traversal);
    let (batch, _) = complete(WIDTH, true, 32, usize::MAX);
    let allocation = allocation.expect("an allocation while installing the selected membership");

    assert!(
        selected > traversal + WIDTH * 2,
        "{traversal} ordinary visits, {selected} with membership installation"
    );
    assert!(batch < selected / 8);
    eprintln!("selected stored membership with {WIDTH} items: unchanged traversal {traversal}, one-step {selected}, 32-step {batch}");
    for interrupted in [0, 1, 2] {
        let native = MemoryBudget::new(8 << 20);
        let work = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut job = input(WIDTH, true, &native, &work, &original, &invoking);
        let single = ImageDecodeWindow { bytes: 1, steps: 1 };
        let before = if interrupted == 2 {
            allocation
        } else {
            traversal + WIDTH
        };
        for _ in 0..before {
            assert!(job
                .advance(&mut std::io::empty(), single)
                .unwrap()
                .is_none());
        }
        let pressure = match interrupted {
            0 => {
                original.cancel();
                None
            }
            1 => {
                invoking.cancel();
                None
            }
            _ => Some(native.reserve(native.limit() - native.used()).unwrap()),
        };
        let error = loop {
            match job.advance(&mut std::io::empty(), single) {
                Ok(None) => (),
                Err(error) => break error,
                Ok(Some(_)) => panic!(
                    "interrupted selected membership completed (action {interrupted} at {before})"
                ),
            }
        };
        assert_eq!(
            error.sqlstate(),
            Some(if interrupted == 2 { "53200" } else { "57014" })
        );
        drop((job, pressure));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}
