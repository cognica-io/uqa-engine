//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic path automata. Construction interns each reachable epsilon closure once.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use super::{
    build_nfa, epsilon_closure_unchecked, simplify, validate_nfa, Nfa, RPQBuildError,
    RegularPathExpr, StateId, MAX_DFA_STATES,
};

/// NFA states represented by one DFA state in the public diagnostic view.
pub type DfaState = BTreeSet<StateId>;

#[derive(Debug)]
pub struct Dfa {
    pub start: DfaState,
    pub accepts: BTreeSet<DfaState>,
    pub transitions: BTreeMap<DfaState, BTreeMap<String, DfaState>>,
}

struct State {
    accepting: bool,
    transitions: BTreeMap<String, StateId>,
}

pub(crate) struct CompiledDfa {
    states: Vec<State>,
}

impl CompiledDfa {
    pub(crate) const START: StateId = 0;

    pub(crate) fn from_path(path: &RegularPathExpr) -> Result<Self, RPQBuildError> {
        let nfa = build_nfa(&simplify(path)?)?;
        let (compiled, _) = determinize(&nfa)?;
        Ok(compiled)
    }

    pub(crate) fn accepting(&self, id: StateId) -> bool {
        self.states[id as usize].accepting
    }

    pub(crate) fn transitions(&self, id: StateId) -> &BTreeMap<String, StateId> {
        &self.states[id as usize].transitions
    }
}

fn determinize(nfa: &Nfa) -> Result<(CompiledDfa, Vec<Rc<DfaState>>), RPQBuildError> {
    validate_nfa(nfa)?;
    let start = Rc::new(epsilon_closure_unchecked(nfa, BTreeSet::from([nfa.start])));
    let mut ids = BTreeMap::from([(Rc::clone(&start), CompiledDfa::START)]);
    let mut subsets = vec![start];
    let mut states = Vec::new();

    while states.len() < subsets.len() {
        let subset = &subsets[states.len()];
        let accepting = subset.contains(&nfa.accept);
        let mut by_label: BTreeMap<&str, BTreeSet<StateId>> = BTreeMap::new();
        for &source in subset.iter() {
            for edge in &nfa.transitions[source as usize] {
                if let Some(label) = &edge.label {
                    by_label.entry(label).or_default().insert(edge.target);
                }
            }
        }

        let mut transitions = BTreeMap::new();
        for (label, targets) in by_label {
            let closed = epsilon_closure_unchecked(nfa, targets);
            let target = if let Some(&id) = ids.get(&closed) {
                id
            } else {
                if subsets.len() >= MAX_DFA_STATES {
                    return Err(RPQBuildError::DfaStateLimitExceeded {
                        limit: MAX_DFA_STATES,
                    });
                }
                let id = subsets.len() as StateId;
                let subset = Rc::new(closed);
                ids.insert(Rc::clone(&subset), id);
                subsets.push(subset);
                id
            };
            transitions.insert(label.to_owned(), target);
        }
        states.push(State {
            accepting,
            transitions,
        });
    }
    Ok((CompiledDfa { states }, subsets))
}

/// Convert the NFA to its public subset-labelled DFA representation.
pub fn subset_construction(nfa: &Nfa) -> Result<Dfa, RPQBuildError> {
    let (program, subsets) = determinize(nfa)?;
    let start = subsets[CompiledDfa::START as usize].as_ref().clone();
    let mut accepts = BTreeSet::new();
    let mut transitions = BTreeMap::new();
    for (id, row) in program.states.into_iter().enumerate() {
        let subset = subsets[id].as_ref();
        if row.accepting {
            accepts.insert(subset.clone());
        }
        let targets = row
            .transitions
            .into_iter()
            .map(|(label, next)| (label, subsets[next as usize].as_ref().clone()))
            .collect();
        transitions.insert(subset.clone(), targets);
    }
    Ok(Dfa {
        start,
        accepts,
        transitions,
    })
}

#[cfg(test)]
mod tests;
