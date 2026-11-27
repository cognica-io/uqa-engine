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
