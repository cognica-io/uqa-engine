//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Congruent alternate segmentation reuses existing arcs in their reference insertion order.

use uqa_core::memory::BudgetedVec;

use super::{viterbi::State, word};
use crate::kuromoji::error::{check_limit, invalid};
use crate::morphology::lattice::{Node, WordId};
use crate::AnalysisResult;

struct ForwardArc {
    end: usize,
    word: WordId,
}

pub(super) fn prune(
    state: &mut State<'_>,
    start: usize,
    end: usize,
    best_start: usize,
) -> AnalysisResult<()> {
    let mut forward = BudgetedVec::new(state.budget);
    forward.reserve(end - start)?;
    for _ in start..end {
        state.resegment_tick()?;
        forward.push(BudgetedVec::new(state.budget))?;
    }
    let mut arcs = 0_usize;
    for position in (start + 1..=end).rev() {
        state.resegment_tick()?;
        for index in 0..state.traversal.lattice.get(position).len() {
            state.resegment_tick()?;
            let node = state.traversal.lattice.get(position)[index];
            if node.back_pos >= start {
                arcs = arcs
                    .checked_add(1)
                    .ok_or_else(|| invalid("Kuromoji resegmentation", "arc count overflow"))?;
                check_limit(
                    "Kuromoji resegmentation arcs",
                    arcs,
                    state.limits.max_resegmentation_arcs,
                )?;
                forward[node.back_pos - start].push(ForwardArc {
                    end: position,
                    word: node.word,
                })?;
            }
        }
        state.traversal.lattice.clear_position(position);
    }
    for position in start..end {
        state.resegment_tick()?;
        if state.traversal.lattice.get(position).is_empty() {
            continue;
        }
        for arc in forward[position - start].iter() {
            state.resegment_tick()?;
            if position == start {
                let predecessor = state.traversal.lattice.get(start)[best_start];
                let right = if start == 0 {
                    0
                } else {
                    word::costs(predecessor.word, state.model, state.user).right
                };
                let word = word::costs(arc.word, state.model, state.user);
                let cost = predecessor
                    .cost
                    .wrapping_add(word.word)
                    .wrapping_add(i32::from(
                        state
                            .model
                            .connection_cost(right, word.left)
                            .expect("validated contexts"),
                    ))
                    .wrapping_add(state.penalty(start, arc.end - start)?);
                state.traversal.lattice.push(
                    arc.end,
                    Node {
                        cost,
                        right: word.right,
                        back_pos: start,
                        word_pos: start,
                        back_index: best_start,
                        word: arc.word,
                    },
                    state.traversal.poll,
                )?;
            } else {
                state.add(position, arc.end, arc.word, true)?;
            }
        }
    }
    Ok(())
}
