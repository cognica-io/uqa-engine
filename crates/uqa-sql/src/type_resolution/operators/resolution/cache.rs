//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process-owned builtin selections occupy fixed storage and borrow immutable catalog types.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::RwLock;

use super::{scalar_catalog_type, ColumnType, SIGNATURES};
use crate::type_resolution::overload_resolution::builtin_canonical_type_name;

const RETAINED_SELECTIONS: usize = 1 << 14;
const OPERATOR_NAME_BYTES: usize = {
    let mut maximum = 0;
    let mut index = 0;
    while index < SIGNATURES.len() {
        let length = SIGNATURES[index].0.len();
        if length > maximum {
            maximum = length;
        }
        index += 1;
    }
    maximum
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct OperandType(Option<&'static ColumnType>);

impl Hash for OperandType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.map(std::mem::discriminant).hash(state);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct SelectionKey {
    name: [u8; OPERATOR_NAME_BYTES],
    length: usize,
    left: OperandType,
    right: OperandType,
}

fn canonical(ty: &ColumnType) -> Option<&'static ColumnType> {
    scalar_catalog_type(builtin_canonical_type_name(ty)?)
}

pub(super) fn selection_key(
    name: &str,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Option<SelectionKey> {
    if name.len() > OPERATOR_NAME_BYTES {
        return None;
    }
    let operand = |ty: Option<&ColumnType>| match ty {
        None => Some(OperandType(None)),
        Some(ty) => canonical(ty).map(|ty| OperandType(Some(ty))),
    };
    let mut bytes = [0; OPERATOR_NAME_BYTES];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    Some(SelectionKey {
        name: bytes,
        length: name.len(),
        left: operand(left)?,
        right: operand(right)?,
    })
}

#[derive(Clone, Copy)]
struct Selection {
    key: SelectionKey,
    types: [&'static ColumnType; 3],
}

pub(super) type BuiltinSelections<const CAPACITY: usize = RETAINED_SELECTIONS> =
    RwLock<SelectionTable<CAPACITY>>;

pub(super) struct SelectionTable<const CAPACITY: usize> {
    slots: [Option<Selection>; CAPACITY],
    len: usize,
}

impl<const CAPACITY: usize> SelectionTable<CAPACITY> {
    pub(super) const fn new() -> Self {
        assert!(CAPACITY > 0);
        Self {
            slots: [None; CAPACITY],
            len: 0,
        }
    }

    fn index(key: &SelectionKey) -> usize {
        let mut hash = DefaultHasher::new();
        key.hash(&mut hash);
        usize::try_from(hash.finish() % u64::try_from(CAPACITY).expect("cache capacity fits u64"))
            .expect("slot index fits usize")
    }

    pub(super) fn get(&self, key: &SelectionKey) -> Option<[&'static ColumnType; 3]> {
        let start = Self::index(key);
        for offset in 0..CAPACITY {
            let entry = self.slots[(start + offset) % CAPACITY]?;
            if entry.key == *key {
                return Some(entry.types);
            }
        }
        None
    }

    fn insert(&mut self, key: SelectionKey, types: [&'static ColumnType; 3]) {
        // Preserve the previous full-cache policy without allocating replacement storage.
        if self.len == CAPACITY {
            self.slots.fill(None);
            self.len = 0;
        }
        let start = Self::index(&key);
        for offset in 0..CAPACITY {
            let slot = &mut self.slots[(start + offset) % CAPACITY];
            match slot {
                Some(entry) if entry.key == key => return,
                Some(_) => {}
                None => {
                    *slot = Some(Selection { key, types });
                    self.len += 1;
                    return;
                }
            }
        }
        unreachable!("a non-full cache has an empty slot")
    }
}

impl<const CAPACITY: usize> Default for SelectionTable<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn retain_selection<const CAPACITY: usize>(
    cache: &BuiltinSelections<CAPACITY>,
    key: SelectionKey,
    selected: &[ColumnType; 3],
) {
    // Only immutable scalar catalog types can survive the request. A polymorphic result keeps its ordinary controlled selection path.
    let Some(left) = canonical(&selected[0]).filter(|ty| **ty == selected[0]) else {
        return;
    };
    let Some(right) = canonical(&selected[1]).filter(|ty| **ty == selected[1]) else {
        return;
    };
    let Some(output) = canonical(&selected[2]).filter(|ty| **ty == selected[2]) else {
        return;
    };
    if let Ok(mut selections) = cache.write() {
        selections.insert(key, [left, right, output]);
    }
}

#[cfg(test)]
mod tests;
