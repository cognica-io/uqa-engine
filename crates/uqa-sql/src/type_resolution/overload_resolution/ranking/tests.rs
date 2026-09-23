//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    identity: usize,
    types: Vec<String>,
    raw_exact: usize,
    exact: usize,
    preferred: usize,
    variadic: bool,
}

impl RankedFunctionMatch for Candidate {
    fn argument_types(&self) -> &[String] {
        &self.types
    }
    fn raw_exact_matches(&self) -> usize {
        self.raw_exact
    }
    fn exact_matches(&self) -> usize {
        self.exact
    }
    fn preferred_matches(&self) -> usize {
        self.preferred
    }
    fn is_variadic_expansion(&self) -> bool {
        self.variadic
    }
}

fn candidate(identity: usize, types: &[&str]) -> Candidate {
    Candidate {
        identity,
        types: types.iter().map(|ty| (*ty).into()).collect(),
        raw_exact: 0,
        exact: 0,
        preferred: 0,
        variadic: false,
    }
}

#[test]
fn raw_exact_and_fixed_shadowing_need_no_signature_copy_allowance() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let mut fixed = candidate(1, &["integer"]);
    fixed.raw_exact = 1;
    let mut variadic = fixed.clone();
    variadic.identity = 2;
    variadic.variadic = true;
    let later = fixed.clone();
    let mut candidates = vec![variadic.clone(), fixed, variadic, later];
    assert!(rank_function_matches_with_control(
        &mut candidates,
        &[Some(ColumnType::Integer)],
        &control
    )
    .unwrap());
    assert_eq!(
        candidates
            .iter()
            .map(|item| item.identity)
            .collect::<Vec<_>>(),
        [1, 1]
    );
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.peak(), 0);
}

#[test]
fn controlled_unknown_ranking_keeps_category_precedence_and_candidate_order() {
    let budget = MemoryBudget::new(1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let cases = [
        (
            vec![
                candidate(1, &["numeric"]),
                candidate(2, &["varchar"]),
                candidate(3, &["text"]),
                candidate(4, &["text"]),
            ],
            vec![None],
            true,
            vec![3, 4],
        ),
        (
            vec![
                candidate(1, &["boolean", "text"]),
                candidate(2, &["numeric", "numeric"]),
            ],
            vec![None, None],
            false,
            vec![1, 2],
        ),
        (
            vec![
                candidate(1, &["numeric", "numeric"]),
                candidate(2, &["numeric", "boolean"]),
            ],
            vec![Some(ColumnType::Integer), None],
            false,
            vec![1, 2],
        ),
    ];
    for (mut candidates, types, expected, identities) in cases {
        let mut ordinary = candidates.clone();
        assert_eq!(rank_function_matches(&mut ordinary, &types), expected);
        assert_eq!(
            rank_function_matches_with_control(&mut candidates, &types, &control).unwrap(),
            expected
        );
        assert_eq!(candidates, ordinary);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.identity)
                .collect::<Vec<_>>(),
            identities
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn rank_workspace_errors_are_distinct_from_ambiguity_and_release_partial_names() {
    for limit in [0, 1, 4, 16] {
        let budget = MemoryBudget::new(limit);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let mut candidates = vec![candidate(1, &["numeric"]), candidate(2, &["varchar"])];
        let result = rank_function_matches_with_control(&mut candidates, &[None], &control);
        if let Err(error) = result {
            let error: crate::SQLError = error.into();
            assert_eq!(error.sqlstate(), Some("53200"));
        } else {
            assert_eq!(candidates[0].identity, 2);
        }
        assert_eq!(budget.used(), 0);
    }
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let mut candidates = vec![candidate(1, &["numeric"]), candidate(2, &["varchar"])];
        let before = candidates.clone();
        let error: crate::SQLError = rank_function_matches_with_control(
            &mut candidates,
            &[None],
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err()
        .into();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(candidates, before);
        assert_eq!(budget.used(), 0);
    }
}
