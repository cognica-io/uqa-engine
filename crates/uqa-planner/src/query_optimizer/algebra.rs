//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Payload-safe algebraic rewrites for operator trees.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use uqa_core::Predicate;
use uqa_operators::OperatorTree;

use super::{tree_map::map_operator_children, QueryOptimizer};

impl QueryOptimizer {
    pub(super) fn simplify_algebra(&self, op: OperatorTree) -> OperatorTree {
        match self.recurse_simplify(op) {
            OperatorTree::Intersect(operands) => {
                if operands.iter().any(OperatorTree::is_empty) {
                    return OperatorTree::Intersect(Vec::new());
                }
                reduce(operands, Boolean::Intersect)
            }
            OperatorTree::Union(mut operands) => {
                operands.retain(|child| !child.is_empty());
                reduce(operands, Boolean::Union)
            }
            other => other,
        }
    }

    pub(super) fn recurse_simplify(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.simplify_algebra(child))
    }
}

#[derive(Clone, Copy)]
enum Boolean {
    Intersect,
    Union,
}

impl Boolean {
    fn opposite_terms<'a>(self, key: &'a MembershipKey<'_>) -> &'a [usize] {
        match (self, key) {
            (Self::Intersect, MembershipKey::Union(ids))
            | (Self::Union, MembershipKey::Intersect(ids)) => ids,
            _ => &[],
        }
    }
}

fn reduce(mut operands: Vec<OperatorTree>, boolean: Boolean) -> OperatorTree {
    if operands.len() > 1 {
        let mut keep = {
            let mut index = MembershipIndex::default();
            let roots: Vec<_> = operands
                .iter()
                .map(|op| {
                    if op.is_membership_only() {
                        index.key(op)
                    } else {
                        None
                    }
                })
                .collect();
            // A root absorbs an opposite Boolean operand only if it also occurs as a child. Such terms already have an interned identity.
            let absorbing: HashSet<_> = roots
                .iter()
                .flatten()
                .filter_map(|key| index.terms.get(key).copied())
                .collect();
            let mut seen = BTreeSet::new();
            roots
                .iter()
                .map(|root| {
                    root.as_ref().is_none_or(|key| {
                        seen.insert(key)
                            && !boolean
                                .opposite_terms(key)
                                .iter()
                                .any(|id| absorbing.contains(id))
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
        };
        operands.retain(|_| keep.next().expect("one decision per operand"));
    }
    if operands.len() == 1 {
        return operands.pop().expect("single operand");
    }
    match boolean {
        Boolean::Intersect => OperatorTree::Intersect(operands),
        Boolean::Union => OperatorTree::Union(operands),
    }
}

/// Predicates retain Value's exact SQL ordering, including equal cross-numeric variants and NaNs. Commutative children are sorted by their canonical identity with multiplicity intact.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum MembershipKey<'a> {
    Empty,
    Filter(&'a str, &'a Predicate, Option<usize>),
    IndexScan(&'a str, &'a str, &'a Predicate),
    Intersect(Vec<usize>),
    Union(Vec<usize>),
    Complement(usize),
    Composed(Vec<usize>),
    VectorExclusion(usize, usize),
}

#[derive(Default)]
struct MembershipIndex<'a> {
    terms: BTreeMap<MembershipKey<'a>, usize>,
}

impl<'a> MembershipIndex<'a> {
    fn id(&mut self, op: &'a OperatorTree) -> Option<usize> {
        let key = self.key(op)?;
        let next = self.terms.len();
        Some(*self.terms.entry(key).or_insert(next))
    }

    fn key(&mut self, op: &'a OperatorTree) -> Option<MembershipKey<'a>> {
        #[cfg(test)]
        KEY_VISITS.with(|visits| visits.set(visits.get() + 1));
        if op.is_empty() {
            return Some(MembershipKey::Empty);
        }
        Some(match op {
            OperatorTree::Filter {
                field,
                predicate,
                source,
            } => MembershipKey::Filter(
                field,
                predicate,
                match source {
                    Some(source) => Some(self.id(source)?),
                    None => None,
                },
            ),
            OperatorTree::IndexScan {
                index_name,
                field,
                predicate,
            } => MembershipKey::IndexScan(index_name, field, predicate),
            OperatorTree::Intersect(children) | OperatorTree::Union(children) => {
                let mut ids: Vec<_> = children
                    .iter()
                    .map(|child| self.id(child))
                    .collect::<Option<_>>()?;
                ids.sort_unstable();
                if matches!(op, OperatorTree::Intersect(_)) {
                    MembershipKey::Intersect(ids)
                } else {
                    MembershipKey::Union(ids)
                }
            }
            OperatorTree::Composed(children) => MembershipKey::Composed(
                children
                    .iter()
                    .map(|child| self.id(child))
                    .collect::<Option<_>>()?,
            ),
            OperatorTree::Complement(inner) => MembershipKey::Complement(self.id(inner)?),
            OperatorTree::VectorExclusion { positive, negative } => {
                MembershipKey::VectorExclusion(self.id(positive)?, self.id(negative)?)
            }
            _ => return None,
        })
    }
}

#[cfg(test)]
thread_local! { static KEY_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Value;

    fn filter(value: i64) -> OperatorTree {
        OperatorTree::Filter {
            field: "value".into(),
            predicate: Predicate::Equals(Value::Int(value)),
            source: None,
        }
    }

    #[test]
    fn wide_absorption_indexes_each_member_once_and_moves_the_surviving_payload() {
        let term = OperatorTree::Term {
            field: Some("body".into()),
            query: "x".repeat(1 << 20),
            scoring: None,
            top_k: None,
        };
        let OperatorTree::Term {
            query: original, ..
        } = &term
        else {
            unreachable!()
        };
        let pointer = original.as_ptr();
        let mut parts = Vec::new();
        for n in 0..128 {
            parts.push(filter(n));
            parts.push(OperatorTree::Union(vec![filter(n + 512), filter(n)]));
        }
        parts.push(term);
        KEY_VISITS.with(|visits| visits.set(0));
        let OperatorTree::Intersect(kept) = reduce(parts, Boolean::Intersect) else {
            panic!("intersection")
        };
        assert_eq!(kept.len(), 129);
        assert_eq!(KEY_VISITS.with(std::cell::Cell::get), 512);
        assert!(
            matches!(kept.last(), Some(OperatorTree::Term { query, .. }) if query.as_ptr() == pointer)
        );
    }
}
