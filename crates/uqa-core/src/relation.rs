//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite-support, document-keyed relations.
//!
//! A [`Relation<K>`] represents a finite-support function `DocId -> K`.
//! Pointwise sum and product are available only when `K` implements
//! [`Semiring`]. This keeps value-combination laws explicit instead of
//! attributing them to the physical posting-list container.

use std::collections::BTreeMap;

use crate::{DocId, DocSet};

/// Value operations required by [`Relation::plus`] and [`Relation::times`].
///
/// Implementations are responsible for the semiring laws. In particular,
/// addition must form a commutative monoid and multiplication must distribute
/// over addition with [`Self::zero`] as an annihilator.
pub trait Semiring: Clone {
    /// Additive identity.
    fn zero() -> Self;

    /// Multiplicative identity.
    fn one() -> Self;

    /// Semiring addition.
    fn plus(&self, other: &Self) -> Self;

    /// Semiring multiplication.
    fn times(&self, other: &Self) -> Self;

    /// Whether this value is the additive identity and therefore outside the
    /// relation's support.
    fn is_zero(&self) -> bool;
}

impl Semiring for bool {
    fn zero() -> Self {
        false
    }

    fn one() -> Self {
        true
    }

    fn plus(&self, other: &Self) -> Self {
        *self || *other
    }

    fn times(&self, other: &Self) -> Self {
        *self && *other
    }

    fn is_zero(&self) -> bool {
        !*self
    }
}

/// A log-space semiring element.
///
/// Values are natural logarithms of non-negative weights. Semiring addition is
/// stable log-sum-exp, multiplication is addition in log space, `-inf` is zero,
/// and `0` is one. `NaN` is rejected at construction because it cannot satisfy
/// the algebraic contract.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogSemiring(f64);

impl LogSemiring {
    /// Construct from a log-space value. Returns `None` for `NaN`.
    pub fn from_log(value: f64) -> Option<Self> {
        (!value.is_nan()).then_some(Self(value))
    }

    /// Construct from a non-negative linear-space weight.
    pub fn from_weight(weight: f64) -> Option<Self> {
        if weight.is_nan() || weight < 0.0 {
            return None;
        }
        if weight == 0.0 {
            return Some(Self::zero());
        }
        Some(Self(weight.ln()))
    }

    /// Return the stored natural logarithm.
    pub fn log_value(self) -> f64 {
        self.0
    }

    /// Return the represented linear-space weight.
    pub fn weight(self) -> f64 {
        self.0.exp()
    }
}

impl Semiring for LogSemiring {
    fn zero() -> Self {
        Self(f64::NEG_INFINITY)
    }

    fn one() -> Self {
        Self(0.0)
    }

    fn plus(&self, other: &Self) -> Self {
        if self.is_zero() {
            return *other;
        }
        if other.is_zero() {
            return *self;
        }
        if self.0 == f64::INFINITY || other.0 == f64::INFINITY {
            return Self(f64::INFINITY);
        }

        let maximum = self.0.max(other.0);
        Self(maximum + ((self.0 - maximum).exp() + (other.0 - maximum).exp()).ln())
    }

    fn times(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            Self::zero()
        } else {
            Self(self.0 + other.0)
        }
    }

    fn is_zero(&self) -> bool {
        self.0 == f64::NEG_INFINITY
    }
}

/// One non-zero value in a [`Relation`].
#[derive(Debug, Clone, PartialEq)]
pub struct RelationEntry<K> {
    pub doc_id: DocId,
    pub value: K,
}

impl<K> RelationEntry<K> {
    pub fn new(doc_id: DocId, value: K) -> Self {
        Self { doc_id, value }
    }
}

/// A finite-support function from document ids to semiring values.
///
/// Entries are sorted by `doc_id`, unique by `doc_id`, and never store the
/// semiring zero value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Relation<K> {
    entries: Vec<RelationEntry<K>>,
}

impl<K> Relation<K> {
    /// Construct the empty relation.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Borrow the sorted non-zero entries.
    pub fn entries(&self) -> &[RelationEntry<K>] {
        &self.entries
    }

    /// Look up a value by document id.
    pub fn get(&self, doc_id: DocId) -> Option<&K> {
        self.entries
            .binary_search_by_key(&doc_id, |entry| entry.doc_id)
            .ok()
            .map(|index| &self.entries[index].value)
    }

    /// Project away values and return the finite support.
    pub fn support(&self) -> DocSet {
        DocSet::from_sorted_unchecked(self.entries.iter().map(|entry| entry.doc_id).collect())
    }

    /// Number of non-zero entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the support is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over non-zero entries in document-id order.
    pub fn iter(&self) -> std::slice::Iter<'_, RelationEntry<K>> {
        self.entries.iter()
    }
}

impl<K: Semiring> Relation<K> {
    /// Lift a document set to its characteristic relation, assigning the
    /// semiring multiplicative identity to every supported document.
    pub fn from_support(support: &DocSet) -> Self {
        Self::from_terms(
            support
                .iter()
                .map(|doc_id| RelationEntry::new(doc_id, K::one())),
        )
    }

    /// Construct from possibly unsorted terms, combining duplicate document ids
    /// with semiring addition and discarding zero results.
    pub fn from_terms<I>(terms: I) -> Self
    where
        I: IntoIterator<Item = RelationEntry<K>>,
    {
        let mut values = BTreeMap::<DocId, K>::new();
        for term in terms {
            if term.value.is_zero() {
                continue;
            }
            values
                .entry(term.doc_id)
                .and_modify(|value| *value = value.plus(&term.value))
                .or_insert(term.value);
        }

        let entries = values
            .into_iter()
            .filter_map(|(doc_id, value)| {
                (!value.is_zero()).then_some(RelationEntry { doc_id, value })
            })
            .collect();
        Self { entries }
    }

    /// Construct a singleton relation. A zero value produces the empty
    /// relation.
    pub fn singleton(doc_id: DocId, value: K) -> Self {
        Self::from_terms([RelationEntry::new(doc_id, value)])
    }

    /// Pointwise semiring addition.
    pub fn plus(&self, other: &Self) -> Self {
        let mut entries = Vec::with_capacity(self.len() + other.len());
        let (mut left, mut right) = (0, 0);

        while left < self.len() && right < other.len() {
            match self.entries[left].doc_id.cmp(&other.entries[right].doc_id) {
                std::cmp::Ordering::Less => {
                    entries.push(self.entries[left].clone());
                    left += 1;
                }
                std::cmp::Ordering::Equal => {
                    let value = self.entries[left].value.plus(&other.entries[right].value);
                    if !value.is_zero() {
                        entries.push(RelationEntry::new(self.entries[left].doc_id, value));
                    }
                    left += 1;
                    right += 1;
                }
                std::cmp::Ordering::Greater => {
                    entries.push(other.entries[right].clone());
                    right += 1;
                }
            }
        }

        entries.extend_from_slice(&self.entries[left..]);
        entries.extend_from_slice(&other.entries[right..]);
        Self { entries }
    }

    /// Pointwise semiring multiplication.
    pub fn times(&self, other: &Self) -> Self {
        let mut entries = Vec::with_capacity(self.len().min(other.len()));
        let (mut left, mut right) = (0, 0);

        while left < self.len() && right < other.len() {
            match self.entries[left].doc_id.cmp(&other.entries[right].doc_id) {
                std::cmp::Ordering::Less => left += 1,
                std::cmp::Ordering::Equal => {
                    let value = self.entries[left].value.times(&other.entries[right].value);
                    if !value.is_zero() {
                        entries.push(RelationEntry::new(self.entries[left].doc_id, value));
                    }
                    left += 1;
                    right += 1;
                }
                std::cmp::Ordering::Greater => right += 1,
            }
        }

        Self { entries }
    }
}

impl From<&DocSet> for Relation<bool> {
    fn from(support: &DocSet) -> Self {
        Self::from_support(support)
    }
}

impl From<DocSet> for Relation<bool> {
    fn from(support: DocSet) -> Self {
        Self::from_support(&support)
    }
}

impl<K> IntoIterator for Relation<K> {
    type Item = RelationEntry<K>;
    type IntoIter = std::vec::IntoIter<RelationEntry<K>>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, K> IntoIterator for &'a Relation<K> {
    type Item = &'a RelationEntry<K>;
    type IntoIter = std::slice::Iter<'a, RelationEntry<K>>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{LogSemiring, Relation, RelationEntry, Semiring};
    use crate::DocSet;

    #[test]
    fn boolean_relation_lifts_set_union_and_intersection() {
        let left = Relation::<bool>::from_support(&DocSet::from(vec![1, 3]));
        let right = Relation::<bool>::from_support(&DocSet::from(vec![2, 3]));

        assert_eq!(left.plus(&right).support(), DocSet::from(vec![1, 2, 3]));
        assert_eq!(left.times(&right).support(), DocSet::from(vec![3]));
    }

    #[test]
    fn duplicate_terms_are_combined_and_zero_is_not_stored() {
        let relation = Relation::from_terms([
            RelationEntry::new(1, false),
            RelationEntry::new(2, true),
            RelationEntry::new(2, true),
        ]);

        assert_eq!(relation.support(), DocSet::from(vec![2]));
        assert_eq!(relation.get(2), Some(&true));
    }

    #[test]
    fn log_semiring_uses_log_sum_exp_and_log_space_multiplication() {
        let point_two = LogSemiring::from_weight(0.2).unwrap();
        let point_three = LogSemiring::from_weight(0.3).unwrap();

        let sum = point_two.plus(&point_three);
        let product = point_two.times(&point_three);

        assert!((sum.weight() - 0.5).abs() < 1e-12);
        assert!((product.weight() - 0.06).abs() < 1e-12);
        assert!(LogSemiring::from_log(f64::NAN).is_none());
    }
}
