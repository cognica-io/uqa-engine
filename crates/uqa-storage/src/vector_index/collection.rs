//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live vector registrations and immutable retained collections preserve their distinct allocation owners.

use std::{borrow::Borrow, collections::BTreeMap, sync::Arc};
use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedMapIter, BudgetedString, MemoryReservation},
    FieldName,
};

use super::VectorIndex;
use crate::{
    read_control::StorageReadControl, ReadOnlySnapshot, StorageBackendError, StorageBackendResult,
};

type LiveIndexes = BTreeMap<FieldName, Box<dyn VectorIndex>>;
type RetainedIndexes = Arc<Budgeted<BudgetedMap<FieldName, Box<dyn VectorIndex>>>>;

/// A table's vector registrations. Retained collections own controlled ordered-map nodes and share their names, index handles and original allowance with nested captures. Live mutation requires an explicit fallible borrow; a retained collection never converts itself to an uncharged writable map.
pub struct VectorIndexes(Contents);

enum Contents {
    Live(LiveIndexes),
    Retained {
        indexes: Option<RetainedIndexes>,
        control: StorageReadControl,
    },
}

impl Default for VectorIndexes {
    fn default() -> Self {
        Self(Contents::Live(BTreeMap::new()))
    }
}

impl From<LiveIndexes> for VectorIndexes {
    fn from(indexes: LiveIndexes) -> Self {
        Self(Contents::Live(indexes))
    }
}

impl VectorIndexes {
    pub fn live_mut(&mut self) -> StorageBackendResult<&mut LiveIndexes> {
        match &mut self.0 {
            Contents::Live(indexes) => Ok(indexes),
            Contents::Retained { control, .. } => {
                control.check()?;
                Err(StorageBackendError::Other(
                    "cannot mutate retained vector registrations".into(),
                ))
            }
        }
    }

    pub fn get<Q: Ord + ?Sized>(&self, field: &Q) -> Option<&(dyn VectorIndex + 'static)>
    where
        FieldName: Borrow<Q>,
    {
        let index = match &self.0 {
            Contents::Live(indexes) => indexes.get(field),
            Contents::Retained { indexes, .. } => {
                indexes.as_ref().and_then(|indexes| indexes.get(field))
            }
        };
        index.map(Box::as_ref)
    }

    pub fn contains_key<Q: Ord + ?Sized>(&self, field: &Q) -> bool
    where
        FieldName: Borrow<Q>,
    {
        self.get(field).is_some()
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Contents::Live(indexes) => indexes.len(),
            Contents::Retained { indexes, .. } => {
                indexes.as_ref().map_or(0, |indexes| indexes.len())
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> VectorIndexesIter<'_> {
        VectorIndexesIter(match &self.0 {
            Contents::Live(indexes) => Entries::Live(indexes.iter()),
            Contents::Retained { indexes, .. } => indexes
                .as_ref()
                .map_or(Entries::Empty, |indexes| Entries::Retained(indexes.iter())),
        })
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &FieldName> {
        self.iter().map(|(field, _)| field)
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = &(dyn VectorIndex + 'static)> {
        self.iter().map(|(_, index)| index)
    }

    /// Snapshot live providers under a caller allowance, or share an already retained collection without copying names, nodes or handles. Nested captures preserve the original provider allowance and cancellation boundary.
    pub fn capture(
        source: &dyn VectorIndexSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if let Some(retained) = source.retained_collection()? {
            match &retained.0 {
                Contents::Retained { control, .. } => control.check()?,
                Contents::Live(_) => {
                    return Err(StorageBackendError::Other(
                        "vector provider returned live registrations as a retained collection"
                            .into(),
                    ))
                }
            }
            return Ok(retained);
        }
        let mut retained = RetainedVectorIndexesBuilder::new(control);
        source.visit(&mut |field, index| {
            let (field, memory) = copy_field(field, control)?;
            let index = index.snapshot_with_control(control)?;
            retained.insert_admitted(field, ReadOnlySnapshot::new(index), memory)
        })?;
        retained.finish()
    }
}

impl<Q: Ord + ?Sized> std::ops::Index<&Q> for VectorIndexes
where
    FieldName: Borrow<Q>,
{
    type Output = dyn VectorIndex;
    fn index(&self, field: &Q) -> &Self::Output {
        self.get(field).expect("missing vector index field")
    }
}

pub struct VectorIndexesIter<'a>(Entries<'a>);

#[expect(
    clippy::large_enum_variant,
    reason = "fixed iterator stack avoids unadmitted heap scratch"
)]
enum Entries<'a> {
    Empty,
    Live(std::collections::btree_map::Iter<'a, FieldName, Box<dyn VectorIndex>>),
    Retained(BudgetedMapIter<'a, FieldName, Box<dyn VectorIndex>>),
}

impl<'a> Iterator for VectorIndexesIter<'a> {
    type Item = (&'a FieldName, &'a (dyn VectorIndex + 'static));

    fn next(&mut self) -> Option<Self::Item> {
        let next = match &mut self.0 {
            Entries::Empty => None,
            Entries::Live(iter) => iter.next(),
            Entries::Retained(iter) => iter.next(),
        };
        next.map(|(field, index)| (field, index.as_ref()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            Entries::Empty => (0, Some(0)),
            Entries::Live(iter) => iter.size_hint(),
            Entries::Retained(iter) => iter.size_hint(),
        }
    }
}

impl ExactSizeIterator for VectorIndexesIter<'_> {}
impl std::iter::FusedIterator for VectorIndexesIter<'_> {}

impl<'a> IntoIterator for &'a VectorIndexes {
    type Item = (&'a FieldName, &'a (dyn VectorIndex + 'static));
    type IntoIter = VectorIndexesIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Borrow physical vector registrations without requiring an owned metadata copy.
pub trait VectorIndexSource {
    fn visit(
        &self,
        visitor: &mut dyn FnMut(&str, &dyn VectorIndex) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()>;

    /// An immutable collection may share its original allocation owner. Implementations must never capture or clone a live collection here.
    fn retained_collection(&self) -> StorageBackendResult<Option<VectorIndexes>> {
        Ok(None)
    }
}

impl VectorIndexSource for LiveIndexes {
    fn visit(
        &self,
        visitor: &mut dyn FnMut(&str, &dyn VectorIndex) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        for (field, index) in self {
            visitor(field, index.as_ref())?;
        }
        Ok(())
    }
}

impl VectorIndexSource for VectorIndexes {
    fn visit(
        &self,
        visitor: &mut dyn FnMut(&str, &dyn VectorIndex) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        for (field, index) in self {
            visitor(field, index)?;
        }
        Ok(())
    }

    fn retained_collection(&self) -> StorageBackendResult<Option<VectorIndexes>> {
        match &self.0 {
            Contents::Live(_) => Ok(None),
            Contents::Retained { indexes, control } => {
                control.check()?;
                Ok(Some(Self(Contents::Retained {
                    indexes: indexes.clone(),
                    control: control.clone(),
                })))
            }
        }
    }
}

/// Assemble reconstructed vector indexes without opaque standard-map nodes. Every node, copied name, boxed adapter and shared owner is reserved before allocation.
pub struct RetainedVectorIndexesBuilder {
    indexes: BudgetedMap<FieldName, Box<dyn VectorIndex>>,
    control: StorageReadControl,
}

impl RetainedVectorIndexesBuilder {
    pub fn new(control: &StorageReadControl) -> Self {
        Self {
            indexes: BudgetedMap::new(control.memory()),
            control: control.clone(),
        }
    }

    /// Admit and copy a field name before a reconstruction producer retains it in intermediate or final metadata.
    pub fn copy_field(
        field: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(FieldName, MemoryReservation)> {
        copy_field(field, control)
    }

    /// Adopt an already admitted field name and a provider-owned immutable index. The name lease must cover its actual capacity and belong to this collection's allowance.
    pub fn insert_admitted<T: VectorIndex + 'static>(
        &mut self,
        field: FieldName,
        index: T,
        mut memory: MemoryReservation,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        if !memory.budget().shares_allowance(self.control.memory())
            || memory.bytes() < field.capacity()
        {
            return Err(StorageBackendError::Other(
                "retained vector name lacks its original allowance".into(),
            ));
        }
        if self.indexes.contains_key(&field) {
            return Err(StorageBackendError::Other(
                "retained vector field is registered twice".into(),
            ));
        }
        memory.grow(size_of::<ReadOnlySnapshot<T>>())?;
        let index = ReadOnlySnapshot::from_budgeted(Budgeted::new(index, memory))?;
        let index: Box<dyn VectorIndex> = Box::new(index);
        self.control.check()?;
        self.indexes.insert(field, index)?;
        Ok(())
    }

    pub fn finish(self) -> StorageBackendResult<VectorIndexes> {
        self.control.check()?;
        let indexes = if self.indexes.is_empty() {
            None
        } else {
            Some(
                Budgeted::new(self.indexes, self.control.memory().empty_reservation())
                    .into_shared()?,
            )
        };
        Ok(VectorIndexes(Contents::Retained {
            indexes,
            control: self.control,
        }))
    }
}

fn copy_field(
    field: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<(FieldName, MemoryReservation)> {
    control.check()?;
    let mut name = BudgetedString::new(control.memory());
    name.reserve(field.len())?;
    for (offset, character) in field.chars().enumerate() {
        if offset % 1024 == 0 {
            control.check()?;
        }
        name.push(character)?;
    }
    Ok(name.into_parts())
}

#[cfg(test)]
mod tests;
