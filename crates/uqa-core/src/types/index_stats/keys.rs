//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cmp::Ordering;

#[derive(Debug, Clone)]
pub(super) enum TermIdentity {
    Scalar(String),
    Units(Vec<u16>),
}

#[derive(Clone, Copy)]
pub(super) enum TermRef<'a> {
    Scalar(&'a str),
    Units(&'a [u16]),
}

impl TermIdentity {
    fn as_ref(&self) -> TermRef<'_> {
        match self {
            Self::Scalar(value) => TermRef::Scalar(value),
            Self::Units(value) => TermRef::Units(value),
        }
    }
}

impl<'a> TermRef<'a> {
    fn units(self) -> Units<'a> {
        match self {
            Self::Scalar(value) => Units::Scalar(value.encode_utf16()),
            Self::Units(value) => Units::Raw(value.iter()),
        }
    }

    fn compare(self, other: Self) -> Ordering {
        match (self, other) {
            (Self::Scalar(left), Self::Scalar(right)) if left == right => Ordering::Equal,
            (left, right) => left.units().cmp(right.units()),
        }
    }
}

enum Units<'a> {
    Scalar(std::str::EncodeUtf16<'a>),
    Raw(std::slice::Iter<'a, u16>),
}

impl Iterator for Units<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Scalar(units) => units.next(),
            Self::Raw(units) => units.next().copied(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct TermKey {
    pub(super) field: String,
    pub(super) term: TermIdentity,
}

impl TermKey {
    pub(super) fn compare(&self, field: &str, term: TermRef<'_>) -> Ordering {
        field
            .cmp(&self.field)
            .then_with(|| term.compare(self.term.as_ref()))
    }
}

impl Ord for TermKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.field
            .cmp(&other.field)
            .then_with(|| self.term.as_ref().compare(other.term.as_ref()))
    }
}

impl PartialOrd for TermKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TermKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for TermKey {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct VectorKey {
    pub(super) field: String,
    pub(super) bits: Vec<u32>,
}

impl VectorKey {
    pub(super) fn compare(&self, field: &str, query: &[f32]) -> Ordering {
        field.cmp(&self.field).then_with(|| {
            query
                .iter()
                .map(|value| value.to_bits())
                .cmp(self.bits.iter().copied())
        })
    }
}
