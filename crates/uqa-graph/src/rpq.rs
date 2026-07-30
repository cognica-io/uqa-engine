//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regular path queries: expression AST, parser, algebraic simplifier,
//! Thompson NFA, and subset-construction DFA. Together these power the
//! `RegularPathQuery` operator (see [`crate::operators`]).
//!
//! Grammar:
//! ```text
//!     atom        := label | '(' alternation ')'
//!     star        := atom ('*' | '{' int ',' int '}')*
//!     concat      := star ('/' star)*
//!     alternation := concat ('|' concat)*
//! ```
//! Precedence (low to high): alternation, concatenation, star.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Hard limits keep user-provided path expressions from turning NFA or DFA
/// compilation into an unbounded memory allocation. They are deliberately
/// independent: a compact NFA can still have an exponential DFA.
pub const MAX_RPQ_AST_DEPTH: usize = 256;
pub const MAX_NFA_STATES: usize = 16_384;
pub const MAX_DFA_STATES: usize = 16_384;

/// Regular path expression. Matches UQA behavior for `pattern.RegularPathExpr`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RegularPathExpr {
    /// A single edge label.
    Label(String),
    /// `lhs / rhs`.
    Concat(Box<RegularPathExpr>, Box<RegularPathExpr>),
    /// `lhs | rhs`.
    Alternation(Box<RegularPathExpr>, Box<RegularPathExpr>),
    /// `inner *`.
    KleeneStar(Box<RegularPathExpr>),
    /// `inner { min, max }`.
    Bounded {
        inner: Box<RegularPathExpr>,
        min: u32,
        max: u32,
    },
}

impl RegularPathExpr {
    pub fn label(name: impl Into<String>) -> Self {
        Self::Label(name.into())
    }
    pub fn concat(left: Self, right: Self) -> Self {
        Self::Concat(Box::new(left), Box::new(right))
    }
    pub fn alt(left: Self, right: Self) -> Self {
        Self::Alternation(Box::new(left), Box::new(right))
    }
    pub fn star(inner: Self) -> Self {
        Self::KleeneStar(Box::new(inner))
    }
    pub fn bounded(inner: Self, min: u32, max: u32) -> Self {
        Self::Bounded {
            inner: Box::new(inner),
            min,
            max,
        }
    }
}

// -------------------------------------------------------------------------
// Parser
// -------------------------------------------------------------------------

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RPQParseError {
    #[error("unexpected token at position {position}: {token:?}")]
    Unexpected { position: usize, token: String },
    #[error("unexpected end of expression")]
    Eof,
    #[error("missing closing parenthesis")]
    MissingParen,
    #[error("malformed bounded repetition: {0}")]
    MalformedBound(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RPQBuildError {
    #[error("bounded repetition minimum {min} exceeds maximum {max}")]
    InvalidBound { min: u32, max: u32 },
    #[error("regular path expression depth {depth} exceeds limit {limit}")]
    ExpressionTooDeep { depth: usize, limit: usize },
    #[error("regular path NFA requires {required} states, exceeding limit {limit}")]
    NfaStateLimitExceeded { required: usize, limit: usize },
    #[error("regular path DFA exceeded state limit {limit}")]
    DfaStateLimitExceeded { limit: usize },
    #[error("invalid NFA: {0}")]
    InvalidNfa(String),
    #[error("unable to reserve memory for {states} NFA states")]
    AllocationFailed { states: usize },
}

pub fn parse_rpq(expr: &str) -> Result<RegularPathExpr, RPQParseError> {
    let tokens = tokenize(expr);
    let (result, pos) = parse_alternation(&tokens, 0)?;
    if pos != tokens.len() {
        return Err(RPQParseError::Unexpected {
            position: pos,
            token: tokens[pos].clone(),
        });
    }
    Ok(result)
}

fn tokenize(expr: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if matches!(ch, '(' | ')' | '/' | '|' | '*' | '{' | '}' | ',') {
            tokens.push(ch.to_string());
            i += 1;
        } else {
            let start = i;
            while i < bytes.len() {
                let c = bytes[i] as char;
                if c.is_ascii_whitespace()
                    || matches!(c, '(' | ')' | '/' | '|' | '*' | '{' | '}' | ',')
                {
                    break;
                }
                i += 1;
            }
            tokens.push(expr[start..i].to_string());
        }
    }
    tokens
}

fn parse_alternation(
    tokens: &[String],
    mut pos: usize,
) -> Result<(RegularPathExpr, usize), RPQParseError> {
    let (mut left, p) = parse_concat(tokens, pos)?;
    pos = p;
    while pos < tokens.len() && tokens[pos] == "|" {
        pos += 1;
        let (right, p) = parse_concat(tokens, pos)?;
        pos = p;
        left = RegularPathExpr::alt(left, right);
    }
    Ok((left, pos))
}

fn parse_concat(
    tokens: &[String],
    mut pos: usize,
) -> Result<(RegularPathExpr, usize), RPQParseError> {
    let (mut left, p) = parse_star(tokens, pos)?;
    pos = p;
    while pos < tokens.len() && tokens[pos] == "/" {
        pos += 1;
        let (right, p) = parse_star(tokens, pos)?;
        pos = p;
        left = RegularPathExpr::concat(left, right);
    }
    Ok((left, pos))
}

fn parse_star(
    tokens: &[String],
    mut pos: usize,
) -> Result<(RegularPathExpr, usize), RPQParseError> {
    let (mut expr, p) = parse_atom(tokens, pos)?;
    pos = p;
    while pos < tokens.len() && (tokens[pos] == "*" || tokens[pos] == "{") {
        if tokens[pos] == "*" {
            pos += 1;
            expr = RegularPathExpr::star(expr);
        } else {
            pos += 1;
            let min = tokens
                .get(pos)
                .ok_or_else(|| RPQParseError::MalformedBound("missing min".into()))?
                .parse::<u32>()
                .map_err(|e| RPQParseError::MalformedBound(format!("min: {e}")))?;
            pos += 1;
            if tokens.get(pos).map(String::as_str) != Some(",") {
                return Err(RPQParseError::MalformedBound("expected ','".into()));
            }
            pos += 1;
            let max = tokens
                .get(pos)
                .ok_or_else(|| RPQParseError::MalformedBound("missing max".into()))?
                .parse::<u32>()
                .map_err(|e| RPQParseError::MalformedBound(format!("max: {e}")))?;
            if min > max {
                return Err(RPQParseError::MalformedBound(format!(
                    "min {min} exceeds max {max}"
                )));
            }
            pos += 1;
            if tokens.get(pos).map(String::as_str) != Some("}") {
                return Err(RPQParseError::MalformedBound("expected '}'".into()));
            }
            pos += 1;
            expr = RegularPathExpr::bounded(expr, min, max);
        }
    }
    Ok((expr, pos))
}

fn parse_atom(
    tokens: &[String],
    mut pos: usize,
) -> Result<(RegularPathExpr, usize), RPQParseError> {
    let token = tokens.get(pos).ok_or(RPQParseError::Eof)?;
    if token == "(" {
        pos += 1;
        let (inner, p) = parse_alternation(tokens, pos)?;
        pos = p;
        if tokens.get(pos).map(String::as_str) != Some(")") {
            return Err(RPQParseError::MissingParen);
        }
        pos += 1;
        Ok((inner, pos))
    } else if matches!(token.as_str(), ")" | "/" | "|" | "*" | "{" | "}" | ",") {
        Err(RPQParseError::Unexpected {
            position: pos,
            token: token.clone(),
        })
    } else {
        pos += 1;
        Ok((RegularPathExpr::label(token.clone()), pos))
    }
}

// -------------------------------------------------------------------------
// Simplifier
// -------------------------------------------------------------------------

/// Algebraic simplification (Section 8.2, Paper 2):
/// `a|a -> a`, `(a*)* -> a*`, `a*|a -> a*`, `a*/a* -> a*`, plus
/// canonicalization of alternation operand order.
pub fn simplify(expr: &RegularPathExpr) -> Result<RegularPathExpr, RPQBuildError> {
    // Validate iteratively before entering the recursive rewriter so a
    // programmatically constructed, deeply nested AST cannot overflow the
    // stack before the compiler has a chance to reject it.
    required_nfa_states(expr)?;
    Ok(simplify_validated(expr))
}

fn simplify_validated(expr: &RegularPathExpr) -> RegularPathExpr {
    match expr {
        RegularPathExpr::Label(_) => expr.clone(),
        RegularPathExpr::Alternation(l, r) => {
            let mut left = simplify_validated(l);
            let mut right = simplify_validated(r);
            if left == right {
                return left;
            }
            if let RegularPathExpr::KleeneStar(inner) = &left {
                if **inner == right {
                    return left;
                }
            }
            if let RegularPathExpr::KleeneStar(inner) = &right {
                if **inner == left {
                    return right;
                }
            }
            // Canonical: sort by debug repr.
            let lr = format!("{left:?}");
            let rr = format!("{right:?}");
            if lr > rr {
                std::mem::swap(&mut left, &mut right);
            }
            RegularPathExpr::alt(left, right)
        }
        RegularPathExpr::Concat(l, r) => {
            let left = simplify_validated(l);
            let right = simplify_validated(r);
            if let (RegularPathExpr::KleeneStar(li), RegularPathExpr::KleeneStar(ri)) =
                (&left, &right)
            {
                if li == ri {
                    return left;
                }
            }
            RegularPathExpr::concat(left, right)
        }
        RegularPathExpr::KleeneStar(inner) => {
            let s = simplify_validated(inner);
            if matches!(s, RegularPathExpr::KleeneStar(_)) {
                s
            } else {
                RegularPathExpr::star(s)
            }
        }
        RegularPathExpr::Bounded { inner, min, max } => {
            RegularPathExpr::bounded(simplify_validated(inner), *min, *max)
        }
    }
}

// -------------------------------------------------------------------------
// NFA (Thompson's construction)
// -------------------------------------------------------------------------

pub type StateId = u32;

/// NFA transition target. `Some(label)` is a labeled edge consumed by
/// matching that label; `None` is an epsilon transition.
#[derive(Debug, Clone)]
pub struct NfaTransition {
    pub label: Option<String>,
    pub target: StateId,
}

#[derive(Debug, Default)]
pub struct Nfa {
    /// `transitions[state_id]` lists every outgoing transition from that
    /// state. Indexed densely; unused state ids hold an empty vec.
    pub transitions: Vec<Vec<NfaTransition>>,
    pub start: StateId,
    pub accept: StateId,
}

impl Nfa {
    fn new() -> Self {
        Self {
            transitions: Vec::new(),
            start: 0,
            accept: 0,
        }
    }

    fn new_state(&mut self) -> Result<StateId, RPQBuildError> {
        if self.transitions.len() >= MAX_NFA_STATES {
            return Err(RPQBuildError::NfaStateLimitExceeded {
                required: self.transitions.len().saturating_add(1),
                limit: MAX_NFA_STATES,
            });
        }
        let id = StateId::try_from(self.transitions.len()).map_err(|_| {
            RPQBuildError::NfaStateLimitExceeded {
                required: self.transitions.len().saturating_add(1),
                limit: MAX_NFA_STATES,
            }
        })?;
        self.transitions.push(Vec::new());
        Ok(id)
    }

    fn add_transition(
        &mut self,
        from: StateId,
        label: Option<String>,
        to: StateId,
    ) -> Result<(), RPQBuildError> {
        if usize::try_from(to)
            .ok()
            .is_none_or(|target| target >= self.transitions.len())
        {
            return Err(RPQBuildError::InvalidNfa(format!(
                "transition target {to} is outside {} states",
                self.transitions.len()
            )));
        }
        let state_count = self.transitions.len();
        let transitions = self
            .transitions
            .get_mut(usize::try_from(from).map_err(|_| {
                RPQBuildError::InvalidNfa(format!("transition source {from} is not addressable"))
            })?)
            .ok_or_else(|| {
                RPQBuildError::InvalidNfa(format!(
                    "transition source {from} is outside {state_count} states"
                ))
            })?;
        transitions.push(NfaTransition { label, target: to });
        Ok(())
    }

    pub fn states(&self) -> Result<Vec<StateId>, RPQBuildError> {
        validate_nfa(self)?;
        let end = StateId::try_from(self.transitions.len()).map_err(|_| {
            RPQBuildError::NfaStateLimitExceeded {
                required: self.transitions.len(),
                limit: MAX_NFA_STATES,
            }
        })?;
        Ok((0..end).collect())
    }
}

/// Build an NFA from a regular path expression via Thompson's
/// construction.
pub fn build_nfa(expr: &RegularPathExpr) -> Result<Nfa, RPQBuildError> {
    let required = required_nfa_states(expr)?;
    let mut nfa = Nfa::new();
    nfa.transitions
        .try_reserve_exact(required)
        .map_err(|_| RPQBuildError::AllocationFailed { states: required })?;
    let (start, accept) = build_fragment(&mut nfa, expr)?;
    nfa.start = start;
    nfa.accept = accept;
    Ok(nfa)
}

fn required_nfa_states(expr: &RegularPathExpr) -> Result<usize, RPQBuildError> {
    let mut work = vec![(expr, 1_usize, false)];
    let mut values = Vec::<usize>::new();
    while let Some((current, depth, visited)) = work.pop() {
        if depth > MAX_RPQ_AST_DEPTH {
            return Err(RPQBuildError::ExpressionTooDeep {
                depth,
                limit: MAX_RPQ_AST_DEPTH,
            });
        }
        if !visited {
            work.push((current, depth, true));
            match current {
                RegularPathExpr::Label(_) => {}
                RegularPathExpr::Concat(left, right)
                | RegularPathExpr::Alternation(left, right) => {
                    work.push((right, depth.saturating_add(1), false));
                    work.push((left, depth.saturating_add(1), false));
                }
                RegularPathExpr::KleeneStar(inner) | RegularPathExpr::Bounded { inner, .. } => {
                    work.push((inner, depth.saturating_add(1), false));
                }
            }
            continue;
        }

        let required = match current {
            RegularPathExpr::Label(_) => Some(2),
            RegularPathExpr::Concat(_, _) => {
                let right = values.pop().ok_or_else(|| {
                    RPQBuildError::InvalidNfa("missing concat right fragment".into())
                })?;
                let left = values.pop().ok_or_else(|| {
                    RPQBuildError::InvalidNfa("missing concat left fragment".into())
                })?;
                left.checked_add(right)
            }
            RegularPathExpr::Alternation(_, _) => {
                let right = values.pop().ok_or_else(|| {
                    RPQBuildError::InvalidNfa("missing alternation right fragment".into())
                })?;
                let left = values.pop().ok_or_else(|| {
                    RPQBuildError::InvalidNfa("missing alternation left fragment".into())
                })?;
                left.checked_add(right).and_then(|sum| sum.checked_add(2))
            }
            RegularPathExpr::KleeneStar(_) => values
                .pop()
                .ok_or_else(|| RPQBuildError::InvalidNfa("missing Kleene-star fragment".into()))?
                .checked_add(2),
            RegularPathExpr::Bounded { min, max, .. } => {
                if min > max {
                    return Err(RPQBuildError::InvalidBound {
                        min: *min,
                        max: *max,
                    });
                }
                let inner = values.pop().ok_or_else(|| {
                    RPQBuildError::InvalidNfa("missing bounded-repeat fragment".into())
                })?;
                usize::try_from(*max)
                    .ok()
                    .and_then(|copies| inner.checked_mul(copies))
                    .and_then(|states| states.checked_add(2))
            }
        }
        .ok_or(RPQBuildError::NfaStateLimitExceeded {
            required: usize::MAX,
            limit: MAX_NFA_STATES,
        })?;
        if required > MAX_NFA_STATES {
            return Err(RPQBuildError::NfaStateLimitExceeded {
                required,
                limit: MAX_NFA_STATES,
            });
        }
        values.push(required);
    }
    values
        .pop()
        .ok_or_else(|| RPQBuildError::InvalidNfa("regular path expression has no fragment".into()))
}

fn build_fragment(
    nfa: &mut Nfa,
    expr: &RegularPathExpr,
) -> Result<(StateId, StateId), RPQBuildError> {
    match expr {
        RegularPathExpr::Label(name) => {
            let s = nfa.new_state()?;
            let a = nfa.new_state()?;
            nfa.add_transition(s, Some(name.clone()), a)?;
            Ok((s, a))
        }
        RegularPathExpr::Concat(l, r) => {
            let (ls, la) = build_fragment(nfa, l)?;
            let (rs, ra) = build_fragment(nfa, r)?;
            nfa.add_transition(la, None, rs)?;
            Ok((ls, ra))
        }
        RegularPathExpr::Alternation(l, r) => {
            let s = nfa.new_state()?;
            let a = nfa.new_state()?;
            let (ls, la) = build_fragment(nfa, l)?;
            let (rs, ra) = build_fragment(nfa, r)?;
            nfa.add_transition(s, None, ls)?;
            nfa.add_transition(s, None, rs)?;
            nfa.add_transition(la, None, a)?;
            nfa.add_transition(ra, None, a)?;
            Ok((s, a))
        }
        RegularPathExpr::KleeneStar(inner) => {
            let s = nfa.new_state()?;
            let a = nfa.new_state()?;
            let (is, ia) = build_fragment(nfa, inner)?;
            nfa.add_transition(s, None, is)?;
            nfa.add_transition(s, None, a)?;
            nfa.add_transition(ia, None, is)?;
            nfa.add_transition(ia, None, a)?;
            Ok((s, a))
        }
        RegularPathExpr::Bounded { inner, min, max } => {
            if min > max {
                return Err(RPQBuildError::InvalidBound {
                    min: *min,
                    max: *max,
                });
            }
            let start = nfa.new_state()?;
            let mut current_end = start;
            for _ in 0..*min {
                let (is, ia) = build_fragment(nfa, inner)?;
                nfa.add_transition(current_end, None, is)?;
                current_end = ia;
            }
            let accept = nfa.new_state()?;
            if min == max {
                nfa.add_transition(current_end, None, accept)?;
            } else {
                nfa.add_transition(current_end, None, accept)?;
                for _ in 0..(*max - *min) {
                    let (is, ia) = build_fragment(nfa, inner)?;
                    nfa.add_transition(current_end, None, is)?;
                    nfa.add_transition(ia, None, accept)?;
                    current_end = ia;
                }
            }
            Ok((start, accept))
        }
    }
}

/// Epsilon closure of a state set: every state reachable by zero or
/// more epsilon (`label == None`) transitions.
pub fn epsilon_closure(
    nfa: &Nfa,
    states: &BTreeSet<StateId>,
) -> Result<BTreeSet<StateId>, RPQBuildError> {
    validate_nfa(nfa)?;
    let mut closure = states.clone();
    let mut stack: Vec<StateId> = states.iter().copied().collect();
    while let Some(s) = stack.pop() {
        let outgoing = nfa
            .transitions
            .get(usize::try_from(s).map_err(|_| {
                RPQBuildError::InvalidNfa(format!("closure state {s} is not addressable"))
            })?)
            .ok_or_else(|| {
                RPQBuildError::InvalidNfa(format!("closure state {s} is outside the NFA"))
            })?;
        for t in outgoing {
            if t.label.is_none() && !closure.contains(&t.target) {
                closure.insert(t.target);
                stack.push(t.target);
            }
        }
    }
    Ok(closure)
}

fn validate_nfa(nfa: &Nfa) -> Result<(), RPQBuildError> {
    let state_count = nfa.transitions.len();
    if state_count == 0 {
        return Err(RPQBuildError::InvalidNfa("NFA has no states".into()));
    }
    if state_count > MAX_NFA_STATES {
        return Err(RPQBuildError::NfaStateLimitExceeded {
            required: state_count,
            limit: MAX_NFA_STATES,
        });
    }
    for (name, state) in [("start", nfa.start), ("accept", nfa.accept)] {
        if usize::try_from(state)
            .ok()
            .is_none_or(|index| index >= state_count)
        {
            return Err(RPQBuildError::InvalidNfa(format!(
                "{name} state {state} is outside {state_count} states"
            )));
        }
    }
    for (source, transitions) in nfa.transitions.iter().enumerate() {
        for transition in transitions {
            if usize::try_from(transition.target)
                .ok()
                .is_none_or(|target| target >= state_count)
            {
                return Err(RPQBuildError::InvalidNfa(format!(
                    "transition from state {source} targets missing state {}",
                    transition.target
                )));
            }
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// DFA (subset construction)
// -------------------------------------------------------------------------

pub type DfaState = BTreeSet<StateId>;

#[derive(Debug)]
pub struct Dfa {
    pub start: DfaState,
    pub accepts: BTreeSet<DfaState>,
    pub transitions: BTreeMap<DfaState, BTreeMap<String, DfaState>>,
}

/// Convert an NFA to a DFA via the standard subset construction.
pub fn subset_construction(nfa: &Nfa) -> Result<Dfa, RPQBuildError> {
    validate_nfa(nfa)?;
    // Collect alphabet (non-epsilon transition labels).
    let mut alphabet: BTreeSet<String> = BTreeSet::new();
    for transitions in &nfa.transitions {
        for t in transitions {
            if let Some(label) = &t.label {
                alphabet.insert(label.clone());
            }
        }
    }

    let initial = epsilon_closure(nfa, &BTreeSet::from([nfa.start]))?;
    let mut transitions: BTreeMap<DfaState, BTreeMap<String, DfaState>> = BTreeMap::new();
    let mut accepts: BTreeSet<DfaState> = BTreeSet::new();
    let mut seen: BTreeSet<DfaState> = BTreeSet::from([initial.clone()]);
    let mut work: VecDeque<DfaState> = VecDeque::from([initial.clone()]);

    if initial.contains(&nfa.accept) {
        accepts.insert(initial.clone());
    }

    while let Some(current) = work.pop_front() {
        let mut step: BTreeMap<String, DfaState> = BTreeMap::new();
        for label in &alphabet {
            let mut next_nfa: BTreeSet<StateId> = BTreeSet::new();
            for sid in &current {
                let outgoing = nfa
                    .transitions
                    .get(usize::try_from(*sid).map_err(|_| {
                        RPQBuildError::InvalidNfa(format!("DFA state {sid} is not addressable"))
                    })?)
                    .ok_or_else(|| {
                        RPQBuildError::InvalidNfa(format!("DFA state {sid} is outside the NFA"))
                    })?;
                for t in outgoing {
                    if t.label.as_deref() == Some(label.as_str()) {
                        next_nfa.insert(t.target);
                    }
                }
            }
            if next_nfa.is_empty() {
                continue;
            }
            let closed = epsilon_closure(nfa, &next_nfa)?;
            step.insert(label.clone(), closed.clone());
            if !seen.contains(&closed) {
                if seen.len() >= MAX_DFA_STATES {
                    return Err(RPQBuildError::DfaStateLimitExceeded {
                        limit: MAX_DFA_STATES,
                    });
                }
                seen.insert(closed.clone());
                work.push_back(closed.clone());
                if closed.contains(&nfa.accept) {
                    accepts.insert(closed);
                }
            }
        }
        transitions.insert(current, step);
    }

    Ok(Dfa {
        start: initial,
        accepts,
        transitions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_label() {
        let e = parse_rpq("knows").unwrap();
        assert_eq!(e, RegularPathExpr::label("knows"));
    }

    #[test]
    fn parse_concat() {
        let e = parse_rpq("knows/likes").unwrap();
        assert_eq!(
            e,
            RegularPathExpr::concat(
                RegularPathExpr::label("knows"),
                RegularPathExpr::label("likes")
            )
        );
    }

    #[test]
    fn parse_alternation_lower_prec_than_concat() {
        let e = parse_rpq("a/b|c").unwrap();
        // a/b first, then alternated with c.
        assert_eq!(
            e,
            RegularPathExpr::alt(
                RegularPathExpr::concat(RegularPathExpr::label("a"), RegularPathExpr::label("b")),
                RegularPathExpr::label("c")
            )
        );
    }

    #[test]
    fn parse_star_binds_tightest() {
        let e = parse_rpq("a*").unwrap();
        assert_eq!(e, RegularPathExpr::star(RegularPathExpr::label("a")));
    }

    #[test]
    fn parse_bounded() {
        let e = parse_rpq("a{2,5}").unwrap();
        assert_eq!(
            e,
            RegularPathExpr::bounded(RegularPathExpr::label("a"), 2, 5)
        );
    }

    #[test]
    fn parse_rejects_reversed_bound() {
        assert!(matches!(
            parse_rpq("a{5,2}"),
            Err(RPQParseError::MalformedBound(message)) if message.contains("exceeds")
        ));
    }

    #[test]
    fn build_rejects_unbounded_state_allocation_before_expansion() {
        let expr = RegularPathExpr::bounded(RegularPathExpr::label("a"), 0, u32::MAX);
        assert!(matches!(
            build_nfa(&expr),
            Err(RPQBuildError::NfaStateLimitExceeded { .. })
        ));
    }

    #[test]
    fn build_rejects_programmatically_reversed_bound() {
        let expr = RegularPathExpr::bounded(RegularPathExpr::label("a"), 5, 2);
        assert_eq!(
            build_nfa(&expr).unwrap_err(),
            RPQBuildError::InvalidBound { min: 5, max: 2 }
        );
    }

    #[test]
    fn subset_construction_rejects_missing_transition_target() {
        let malformed = Nfa {
            transitions: vec![vec![NfaTransition {
                label: None,
                target: 1,
            }]],
            start: 0,
            accept: 0,
        };
        assert!(matches!(
            subset_construction(&malformed),
            Err(RPQBuildError::InvalidNfa(message)) if message.contains("missing state")
        ));
    }

    #[test]
    fn parse_grouping() {
        let e = parse_rpq("(a|b)*").unwrap();
        assert_eq!(
            e,
            RegularPathExpr::star(RegularPathExpr::alt(
                RegularPathExpr::label("a"),
                RegularPathExpr::label("b")
            ))
        );
    }

    #[test]
    fn simplify_idempotent_alternation() {
        let e = RegularPathExpr::alt(RegularPathExpr::label("a"), RegularPathExpr::label("a"));
        assert_eq!(simplify(&e).unwrap(), RegularPathExpr::label("a"));
    }

    #[test]
    fn simplify_nested_kleene() {
        let e = RegularPathExpr::star(RegularPathExpr::star(RegularPathExpr::label("a")));
        assert_eq!(
            simplify(&e).unwrap(),
            RegularPathExpr::star(RegularPathExpr::label("a"))
        );
    }

    #[test]
    fn simplify_star_subsumes_label() {
        let e = RegularPathExpr::alt(
            RegularPathExpr::star(RegularPathExpr::label("a")),
            RegularPathExpr::label("a"),
        );
        assert_eq!(
            simplify(&e).unwrap(),
            RegularPathExpr::star(RegularPathExpr::label("a"))
        );
    }

    #[test]
    fn nfa_label_two_states() {
        let nfa = build_nfa(&RegularPathExpr::label("a")).unwrap();
        assert_eq!(nfa.transitions.len(), 2);
        assert_ne!(nfa.start, nfa.accept);
    }

    #[test]
    fn dfa_recognizes_a_or_b() {
        let nfa = build_nfa(&RegularPathExpr::alt(
            RegularPathExpr::label("a"),
            RegularPathExpr::label("b"),
        ))
        .unwrap();
        let dfa = subset_construction(&nfa).unwrap();
        // After reading 'a' from start, the DFA should reach an accept.
        let after_a = dfa
            .transitions
            .get(&dfa.start)
            .and_then(|m| m.get("a"))
            .expect("no `a` transition");
        assert!(dfa.accepts.contains(after_a));
        let after_b = dfa
            .transitions
            .get(&dfa.start)
            .and_then(|m| m.get("b"))
            .expect("no `b` transition");
        assert!(dfa.accepts.contains(after_b));
        // And no `c`.
        assert!(dfa
            .transitions
            .get(&dfa.start)
            .and_then(|m| m.get("c"))
            .is_none());
    }

    #[test]
    fn dfa_recognizes_kleene_star() {
        let nfa = build_nfa(&RegularPathExpr::star(RegularPathExpr::label("a"))).unwrap();
        let dfa = subset_construction(&nfa).unwrap();
        // Empty string accepted (start is in accepts).
        assert!(dfa.accepts.contains(&dfa.start));
    }
}
