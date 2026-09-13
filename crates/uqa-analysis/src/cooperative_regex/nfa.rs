//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cancellable ordered traversal over the dependency's public Thompson states and look assertions.

use std::ops::Range;

use regex_automata::{
    nfa::thompson::{State, NFA},
    util::primitives::StateID,
};
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError};

use super::{control::Control, states::States, ABSENT};
use crate::AnalysisResult;

#[derive(Clone, Copy)]
enum Pending {
    Visit(StateID),
    Restore { slot: usize, value: usize },
}

pub(super) struct Cache {
    current: States,
    next: States,
    stack: BudgetedVec<Pending>,
    initial: BudgetedVec<usize>,
}

impl Cache {
    pub(super) fn new(
        nfa: &NFA,
        width: usize,
        budget: &MemoryBudget,
        control: &mut Control<'_>,
    ) -> AnalysisResult<Self> {
        // Every state is expanded at most once per input position. Reserve all possible pending branches and capture restores before traversal.
        let mut frames = 1usize;
        for state in nfa.states() {
            control.step()?;
            let additional = match state {
                State::Union { alternates } => alternates.len().saturating_sub(1),
                State::BinaryUnion { .. } | State::Capture { .. } => 1,
                _ => 0,
            };
            frames = frames
                .checked_add(additional)
                .ok_or(MemoryError::SizeOverflow)?;
        }
        let mut stack = BudgetedVec::new(budget);
        stack.reserve(frames)?;
        Ok(Self {
            current: States::new(nfa, width, budget, control)?,
            next: States::new(nfa, width, budget, control)?,
            stack,
            initial: control.buffer(budget, width, ABSENT)?,
        })
    }

    pub(super) fn search(
        &mut self,
        nfa: &NFA,
        text: &str,
        span: Range<usize>,
        anchored: bool,
        result: &mut [usize],
        control: &mut Control<'_>,
    ) -> AnalysisResult<bool> {
        self.current.clear();
        self.next.clear();
        self.stack.clear();
        control.fill(&mut self.initial, ABSENT)?;
        control.fill(result, ABSENT)?;
        let mut matched = false;
        let anchored = anchored || nfa.is_always_start_anchored();
        for at in span.start..=span.end {
            control.step()?;
            if self.current.order.is_empty() && (matched || (anchored && at > span.start)) {
                break;
            }
            let closure = Closure {
                nfa,
                bytes: text.as_bytes(),
                at,
            };
            // Append new start paths after existing paths to preserve leftmost priority. A string regex may only start at a UTF-8 boundary.
            if !matched && (!anchored || at == span.start) && text.is_char_boundary(at) {
                closure.expand(
                    nfa.start_anchored(),
                    &mut self.stack,
                    &mut self.initial,
                    &mut self.current,
                    control,
                )?;
            }
            for index in 0..self.current.order.len() {
                control.step()?;
                let state = self.current.order[index];
                if matches!(nfa.state(state), State::Match { .. }) {
                    control.copy(self.current.slots(state), result)?;
                    matched = true;
                    // Only paths processed before this match can supersede it with a preferred or greedier match.
                    break;
                }
                if at == span.end {
                    continue;
                }
                let target = match nfa.state(state) {
                    State::ByteRange { trans } => {
                        trans.matches(closure.bytes, at).then_some(trans.next)
                    }
                    State::Sparse(trans) => trans.matches(closure.bytes, at),
                    State::Dense(trans) => trans.matches(closure.bytes, at),
                    _ => None,
                };
                if let Some(target) = target {
                    Closure {
                        at: at + 1,
                        ..closure
                    }
                    .expand(
                        target,
                        &mut self.stack,
                        self.current.slots(state),
                        &mut self.next,
                        control,
                    )?;
                }
            }
            std::mem::swap(&mut self.current, &mut self.next);
            self.next.clear();
        }
        Ok(matched)
    }
}

#[derive(Clone, Copy)]
struct Closure<'a> {
    nfa: &'a NFA,
    bytes: &'a [u8],
    at: usize,
}

impl Closure<'_> {
    fn expand(
        &self,
        start: StateID,
        stack: &mut BudgetedVec<Pending>,
        slots: &mut [usize],
        target: &mut States,
        control: &mut Control<'_>,
    ) -> AnalysisResult<()> {
        stack.push(Pending::Visit(start))?;
        while let Some(pending) = stack.pop() {
            control.step()?;
            match pending {
                Pending::Restore { slot, value } => slots[slot] = value,
                Pending::Visit(mut state) => loop {
                    control.step()?;
                    if !target.insert(state)? {
                        break;
                    }
                    match self.nfa.state(state) {
                        State::Look { look, next } => {
                            if !self.nfa.look_matcher().matches(*look, self.bytes, self.at) {
                                break;
                            }
                            state = *next;
                        }
                        State::Union { alternates } => {
                            let Some((&first, rest)) = alternates.split_first() else {
                                break;
                            };
                            for &alternate in rest.iter().rev() {
                                control.step()?;
                                stack.push(Pending::Visit(alternate))?;
                            }
                            state = first;
                        }
                        State::BinaryUnion { alt1, alt2 } => {
                            stack.push(Pending::Visit(*alt2))?;
                            state = *alt1;
                        }
                        State::Capture { next, slot, .. } => {
                            let slot = slot.as_usize();
                            if let Some(value) = slots.get_mut(slot) {
                                stack.push(Pending::Restore {
                                    slot,
                                    value: *value,
                                })?;
                                *value = self.at;
                            }
                            state = *next;
                        }
                        State::Fail => break,
                        State::ByteRange { .. }
                        | State::Sparse(_)
                        | State::Dense(_)
                        | State::Match { .. } => {
                            control.copy(slots, target.slots(state))?;
                            break;
                        }
                    }
                },
            }
        }
        Ok(())
    }
}
