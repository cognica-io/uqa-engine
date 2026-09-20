//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical predicate addresses supplied by execution, independent of physical record layouts.

use std::ops::Bound;

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use super::{VersionError, VersionResult};

/// Row identities and ordered index keys are separate logical address spaces. Index identities must describe immutable incarnations, not reusable names or catalog OIDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializableKeySpace {
    Rows,
    Index([u8; 16]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection<'a> {
    Object,
    Point(SerializableKeySpace, &'a [u8]),
    Range(SerializableKeySpace, Bound<&'a [u8]>, Bound<&'a [u8]>),
}

/// A logical read predicate or write address in one immutable object incarnation. Execution supplies order-preserving key bytes with the selected index's comparison/collation semantics; storage never interprets SQL values or physical posting records. An object predicate covers every key space belonging to that object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerializablePredicate<'a> {
    pub(super) object: [u8; 16],
    selection: Selection<'a>,
}

impl<'a> SerializablePredicate<'a> {
    pub const fn object(object: [u8; 16]) -> Self {
        Self {
            object,
            selection: Selection::Object,
        }
    }

    pub const fn point(object: [u8; 16], space: SerializableKeySpace, key: &'a [u8]) -> Self {
        Self {
            object,
            selection: Selection::Point(space, key),
        }
    }

    /// Bounds describe the actually observed key interval, including absent results, not just returned rows. Equal inclusive bounds observe one key; reversed or exclusively equal bounds observe an empty interval.
    pub const fn range(
        object: [u8; 16],
        space: SerializableKeySpace,
        lower: Bound<&'a [u8]>,
        upper: Bound<&'a [u8]>,
    ) -> Self {
        Self {
            object,
            selection: Selection::Range(space, lower, upper),
        }
    }

    pub(super) fn validate(self, writing: bool) -> VersionResult<()> {
        if self.object == [0; 16] {
            return Err(VersionError::InvalidEncoding(
                "serializable predicate requires an immutable object identity",
            ));
        }
        let space = match self.selection {
            Selection::Object => None,
            Selection::Point(space, _) => Some(space),
            Selection::Range(space, ..) if !writing => Some(space),
            Selection::Range(..) => {
                return Err(VersionError::InvalidEncoding(
                    "serializable writes require logical points or an entire object",
                ));
            }
        };
        if space == Some(SerializableKeySpace::Index([0; 16])) {
            return Err(VersionError::InvalidEncoding(
                "serializable predicate requires an immutable index identity",
            ));
        }
        Ok(())
    }

    pub(super) fn is_empty(self) -> bool {
        let Selection::Range(_, lower, upper) = self.selection else {
            return false;
        };
        match (lower, upper) {
            (Bound::Unbounded, Bound::Excluded(right)) => right.is_empty(),
            (Bound::Unbounded, _) | (_, Bound::Unbounded) => false,
            (Bound::Included(left), Bound::Included(right)) => left > right,
            (Bound::Excluded(left), Bound::Excluded(right)) => {
                left >= right || right.strip_prefix(left) == Some(&[0][..])
            }
            (
                Bound::Included(left) | Bound::Excluded(left),
                Bound::Included(right) | Bound::Excluded(right),
            ) => left >= right,
        }
    }

    /// The writer was validated as a point or an object observation.
    pub(super) fn overlaps_write(self, writer: Self) -> bool {
        if self.object != writer.object || self.is_empty() {
            return false;
        }
        match (self.selection, writer.selection) {
            (Selection::Object, _) | (_, Selection::Object) => true,
            (Selection::Point(space, key), Selection::Point(write_space, write_key)) => {
                space == write_space && key == write_key
            }
            (Selection::Range(space, lower, upper), Selection::Point(write_space, key)) => {
                space == write_space && after_lower(key, lower) && before_upper(key, upper)
            }
            (_, Selection::Range(..)) => false,
        }
    }
}

fn after_lower(key: &[u8], lower: Bound<&[u8]>) -> bool {
    match lower {
        Bound::Unbounded => true,
        Bound::Included(bound) => key >= bound,
        Bound::Excluded(bound) => key > bound,
    }
}

fn before_upper(key: &[u8], upper: Bound<&[u8]>) -> bool {
    match upper {
        Bound::Unbounded => true,
        Bound::Included(bound) => key <= bound,
        Bound::Excluded(bound) => key < bound,
    }
}

pub(super) struct OwnedPredicate {
    pub(super) object: [u8; 16],
    pub(super) space: Option<SerializableKeySpace>,
    pub(super) point: bool,
    pub(super) lower: Bound<BudgetedVec<u8>>,
    pub(super) upper: Bound<BudgetedVec<u8>>,
}

impl OwnedPredicate {
    pub(super) fn new(
        predicate: SerializablePredicate<'_>,
        memory: &MemoryBudget,
    ) -> VersionResult<Self> {
        let (space, point, lower, upper) = match predicate.selection {
            Selection::Object => (None, false, Bound::Unbounded, Bound::Unbounded),
            Selection::Point(space, key) => {
                (Some(space), true, Bound::Included(key), Bound::Unbounded)
            }
            Selection::Range(space, lower, upper) => (Some(space), false, lower, upper),
        };
        Ok(Self {
            object: predicate.object,
            space,
            point,
            lower: own_bound(lower, memory)?,
            upper: own_bound(upper, memory)?,
        })
    }

    pub(super) fn borrowed(&self) -> SerializablePredicate<'_> {
        let selection = match self.space {
            None => Selection::Object,
            Some(space) if self.point => {
                let Bound::Included(key) = &self.lower else {
                    unreachable!("an owned point retains its exact key");
                };
                Selection::Point(space, key)
            }
            Some(space) => {
                Selection::Range(space, borrow_bound(&self.lower), borrow_bound(&self.upper))
            }
        };
        SerializablePredicate {
            object: self.object,
            selection,
        }
    }
}

fn own_bound(bound: Bound<&[u8]>, memory: &MemoryBudget) -> VersionResult<Bound<BudgetedVec<u8>>> {
    let key = match bound {
        Bound::Unbounded => return Ok(Bound::Unbounded),
        Bound::Included(key) | Bound::Excluded(key) => key,
    };
    let mut owned = BudgetedVec::new(memory);
    owned.extend_from_slice(key)?;
    Ok(match bound {
        Bound::Included(_) => Bound::Included(owned),
        Bound::Excluded(_) => Bound::Excluded(owned),
        Bound::Unbounded => unreachable!(),
    })
}

fn borrow_bound(bound: &Bound<BudgetedVec<u8>>) -> Bound<&[u8]> {
    match bound {
        Bound::Unbounded => Bound::Unbounded,
        Bound::Included(key) => Bound::Included(key),
        Bound::Excluded(key) => Bound::Excluded(key),
    }
}
