//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[cfg(not(feature = "nori"))]
use crate::kuromoji::KuromojiLimits as Limits;
#[cfg(feature = "nori")]
use crate::nori::NoriLimits as Limits;

fn candidate(position: usize, cost: i32) -> Node {
    Node {
        cost,
        right: 0,
        back_pos: position.saturating_sub(1),
        word_pos: position,
        back_index: 0,
        word: WordId::Known(0),
    }
}

#[test]
fn rolling_single_candidate_positions_allocate_only_during_warmup() {
    let budget = MemoryBudget::new(64 * 1024);
    let mut poll = || Ok(());
    let mut lattice = Lattice::new(Limits::default(), &budget, &mut poll).unwrap();
    lattice.push(1, candidate(1, 11), &mut poll).unwrap();
    lattice.release_before(1, &mut poll).unwrap();
    let retained = budget.used();
    let cost = allocation_counter::measure(|| {
        for position in 2..4098 {
            lattice
                .push(position, candidate(position, 11), &mut poll)
                .unwrap();
            lattice.release_before(position, &mut poll).unwrap();
            assert_eq!(lattice.get(position)[0].word_pos, position);
            assert_eq!(lattice.candidates, 1);
        }
    });
    assert_eq!(cost.count_total, 0, "{cost:?}");
    assert_eq!(budget.used(), retained);
    drop(lattice);
    assert_eq!(budget.used(), 0);
}

#[test]
fn spilling_a_position_preserves_order_and_original_candidate_on_failed_growth() {
    let budget = MemoryBudget::new(64 * 1024);
    let mut poll = || Ok(());
    let mut lattice = Lattice::new(Limits::default(), &budget, &mut poll).unwrap();
    let other = budget.reserve(budget.available()).unwrap();
    let result = lattice.push(0, candidate(1, 11), &mut poll);
    assert!(matches!(result, Err(AnalysisError::Memory(_))));
    assert_eq!(lattice.candidates, 1);
    assert_eq!(lattice.get(0)[0].cost, 0);
    drop(other);
    for i in 1..=8 {
        lattice.push(0, candidate(i, 11), &mut poll).unwrap();
    }
    assert_eq!(
        lattice
            .get(0)
            .iter()
            .map(|node| node.word_pos)
            .collect::<Vec<_>>(),
        (0..=8).collect::<Vec<_>>()
    );
    lattice.prune(0, 0, 5, &mut poll).unwrap();
    assert_eq!(lattice.candidates, 1);
    assert_eq!(lattice.get(0)[0].word_pos, 5);
    lattice.rebase(0);
    assert_eq!(lattice.get(0)[0].cost, 0);
    drop(lattice);
    assert_eq!(budget.used(), 0);
}

#[test]
fn sparse_positions_fit_within_one_candidate_list_header_per_slot() {
    let count = 512;
    let slot_bytes = size_of::<BudgetedVec<Node>>();
    let candidate_bytes = size_of::<Node>();
    let budget = MemoryBudget::new((count + 1) * slot_bytes + candidate_bytes);
    let mut poll = || Ok(());
    let mut lattice = Lattice::new(Limits::default(), &budget, &mut poll).unwrap();
    lattice.ensure(count - 1, &mut poll).unwrap();
    assert_eq!(lattice.candidates, 1);
    assert!(lattice.get(count - 1).is_empty());
    assert!(budget.used() <= count * slot_bytes + candidate_bytes);
    drop(lattice);
    assert_eq!(budget.used(), 0);
}

#[test]
fn ambiguous_positions_return_their_buffers_after_failed_growth_and_release() {
    let budget = MemoryBudget::new(64 * 1024);
    let mut poll = || Ok(());
    let mut lattice = Lattice::new(Limits::default(), &budget, &mut poll).unwrap();
    for pos in 0..3 {
        for candidate_index in lattice.get(pos).len()..4 {
            lattice
                .push(
                    pos,
                    candidate(pos, i32::try_from(candidate_index).unwrap()),
                    &mut poll,
                )
                .unwrap();
        }
        lattice.ensure(pos + 1, &mut poll).unwrap();
    }
    let used = budget.used();
    let occupied = budget.reserve(budget.available()).unwrap();
    assert!(matches!(
        lattice.push(1, candidate(1, 7), &mut poll),
        Err(AnalysisError::Memory(_))
    ));
    drop(occupied);
    assert_eq!(budget.used(), used);
    assert_eq!(lattice.candidates, 12);
    assert_eq!(
        lattice
            .get(1)
            .iter()
            .map(|node| node.cost)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    lattice.release_before(1, &mut poll).unwrap();
    assert_eq!(budget.used(), used - 4 * size_of::<Node>());
    lattice.prune(1, 2, 2, &mut poll).unwrap();
    assert_eq!(lattice.candidates, 1);
    assert_eq!(lattice.get(2)[0].cost, 2);
    assert_eq!(budget.used(), used - 8 * size_of::<Node>());
    lattice.clear_position(2);
    assert_eq!(budget.used(), used - 12 * size_of::<Node>());
    drop(lattice);
    assert_eq!(budget.used(), 0);
}
