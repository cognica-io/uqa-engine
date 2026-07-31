//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document-id support sets.
//!
//! [`DocSet`] is the carrier for document-level Boolean algebra. Payload-bearing
//! posting lists project onto this type through [`crate::PostingList::support`];
//! payload values are deliberately absent, so equality and the Boolean laws
//! describe exactly the same thing.

use std::ops::{BitAnd, BitOr, Sub};

use crate::DocId;

/// A finite set of document ids stored in ascending order.
///
/// The explicit universe passed to [`Self::complement`] determines the finite
/// Boolean algebra in which a complement is evaluated.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocSet {
    doc_ids: Vec<DocId>,
}

impl DocSet {
    /// Construct an empty document set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a document set from ids in arbitrary order.
    pub fn from_unsorted(mut doc_ids: Vec<DocId>) -> Self {
        doc_ids.sort_unstable();
        doc_ids.dedup();
        Self { doc_ids }
    }

    pub(crate) fn from_sorted_unchecked(doc_ids: Vec<DocId>) -> Self {
        debug_assert!(
            doc_ids.windows(2).all(|window| window[0] < window[1]),
            "DocSet::from_sorted_unchecked invariant violated"
        );
        Self { doc_ids }
    }

    /// Set union.
    pub fn union(&self, other: &Self) -> Self {
        let mut result = Vec::with_capacity(self.len() + other.len());
        let (mut left, mut right) = (0, 0);

        while left < self.len() && right < other.len() {
            match self.doc_ids[left].cmp(&other.doc_ids[right]) {
                std::cmp::Ordering::Less => {
                    result.push(self.doc_ids[left]);
                    left += 1;
                }
                std::cmp::Ordering::Equal => {
                    result.push(self.doc_ids[left]);
                    left += 1;
                    right += 1;
                }
                std::cmp::Ordering::Greater => {
                    result.push(other.doc_ids[right]);
                    right += 1;
                }
            }
        }

        result.extend_from_slice(&self.doc_ids[left..]);
        result.extend_from_slice(&other.doc_ids[right..]);
        Self::from_sorted_unchecked(result)
    }

    /// Set intersection.
    pub fn intersect(&self, other: &Self) -> Self {
        let mut result = Vec::with_capacity(self.len().min(other.len()));
        let (mut left, mut right) = (0, 0);

        while left < self.len() && right < other.len() {
            match self.doc_ids[left].cmp(&other.doc_ids[right]) {
                std::cmp::Ordering::Less => left += 1,
                std::cmp::Ordering::Equal => {
                    result.push(self.doc_ids[left]);
                    left += 1;
                    right += 1;
                }
                std::cmp::Ordering::Greater => right += 1,
            }
        }

        Self::from_sorted_unchecked(result)
    }

    /// Set difference, `self \\ other`.
    pub fn difference(&self, other: &Self) -> Self {
        let mut result = Vec::with_capacity(self.len());
        let (mut left, mut right) = (0, 0);

        while left < self.len() && right < other.len() {
            match self.doc_ids[left].cmp(&other.doc_ids[right]) {
                std::cmp::Ordering::Less => {
                    result.push(self.doc_ids[left]);
                    left += 1;
                }
                std::cmp::Ordering::Equal => {
                    left += 1;
                    right += 1;
                }
                std::cmp::Ordering::Greater => right += 1,
            }
        }

        result.extend_from_slice(&self.doc_ids[left..]);
        Self::from_sorted_unchecked(result)
    }

    /// Complement relative to an explicit finite universe.
    pub fn complement(&self, universe: &Self) -> Self {
        universe.difference(self)
    }

    /// Return whether `doc_id` belongs to this set.
    pub fn contains(&self, doc_id: DocId) -> bool {
        self.doc_ids.binary_search(&doc_id).is_ok()
    }

    /// Borrow the sorted document-id representation.
    pub fn as_slice(&self) -> &[DocId] {
        &self.doc_ids
    }

    /// Iterate over document ids in ascending order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = DocId> + DoubleEndedIterator + '_ {
        self.doc_ids.iter().copied()
    }

    /// Number of document ids in the set.
    pub fn len(&self) -> usize {
        self.doc_ids.len()
    }

    /// Whether the set contains no document ids.
    pub fn is_empty(&self) -> bool {
        self.doc_ids.is_empty()
    }
}

impl FromIterator<DocId> for DocSet {
    fn from_iter<I: IntoIterator<Item = DocId>>(iter: I) -> Self {
        Self::from_unsorted(iter.into_iter().collect())
    }
}

impl From<Vec<DocId>> for DocSet {
    fn from(doc_ids: Vec<DocId>) -> Self {
        Self::from_unsorted(doc_ids)
    }
}

impl IntoIterator for DocSet {
    type Item = DocId;
    type IntoIter = std::vec::IntoIter<DocId>;

    fn into_iter(self) -> Self::IntoIter {
        self.doc_ids.into_iter()
    }
}

impl<'a> IntoIterator for &'a DocSet {
    type Item = DocId;
    type IntoIter = std::iter::Copied<std::slice::Iter<'a, DocId>>;

    fn into_iter(self) -> Self::IntoIter {
        self.doc_ids.iter().copied()
    }
}

impl BitOr for &DocSet {
    type Output = DocSet;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl BitAnd for &DocSet {
    type Output = DocSet;

    fn bitand(self, rhs: Self) -> Self::Output {
        self.intersect(rhs)
    }
}

impl Sub for &DocSet {
    type Output = DocSet;

    fn sub(self, rhs: Self) -> Self::Output {
        self.difference(rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::DocSet;

    #[test]
    fn construction_sorts_and_deduplicates() {
        assert_eq!(
            DocSet::from_unsorted(vec![3, 1, 3, 2]).as_slice(),
            &[1, 2, 3]
        );
    }

    #[test]
    fn operations_preserve_set_semantics() {
        let left = DocSet::from(vec![1, 3, 5]);
        let right = DocSet::from(vec![2, 3, 4]);
        let universe = DocSet::from(vec![1, 2, 3, 4, 5]);

        assert_eq!((&left | &right).as_slice(), &[1, 2, 3, 4, 5]);
        assert_eq!((&left & &right).as_slice(), &[3]);
        assert_eq!((&left - &right).as_slice(), &[1, 5]);
        assert_eq!(left.complement(&universe).as_slice(), &[2, 4]);
    }
}
