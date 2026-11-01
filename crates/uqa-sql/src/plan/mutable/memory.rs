//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constructor records for independently replaceable plan fields.

use super::super::scalar::ScalarMemory;
use uqa_core::memory::{BudgetedVec, MemoryReservation};

#[derive(Debug)]
pub(crate) enum RewriteMemory {
    Fields {
        direct: MemoryReservation,
        fields: BudgetedVec<RewriteField>,
    },
    Scalars {
        direct: MemoryReservation,
        items: BudgetedVec<Option<ScalarMemory>>,
    },
    Scalar(Option<ScalarMemory>),
}

#[derive(Debug)]
pub(crate) struct RewriteField {
    pub(crate) name: &'static str,
    pub(crate) memory: Option<RewriteMemory>,
}

impl RewriteMemory {
    pub(super) fn reserved_bytes(&self) -> usize {
        match self {
            Self::Scalar(memory) => memory.as_ref().map_or(0, ScalarMemory::reserved_bytes),
            Self::Scalars { direct, items } => {
                direct.bytes()
                    + items.capacity() * size_of::<Option<ScalarMemory>>()
                    + items
                        .iter()
                        .flatten()
                        .map(ScalarMemory::reserved_bytes)
                        .sum::<usize>()
            }
            Self::Fields { direct, fields } => {
                direct.bytes()
                    + fields.capacity() * size_of::<RewriteField>()
                    + fields
                        .iter()
                        .filter_map(|field| field.memory.as_ref())
                        .map(Self::reserved_bytes)
                        .sum::<usize>()
            }
        }
    }

    pub(crate) fn into_reservation(self) -> Option<MemoryReservation> {
        match self {
            Self::Scalar(memory) => memory.map(ScalarMemory::into_reservation),
            Self::Scalars {
                mut direct,
                mut items,
            } => {
                while let Some(memory) = items.pop() {
                    if let Some(memory) = memory {
                        direct.absorb(memory.into_reservation());
                    }
                }
                drop(items);
                Some(direct)
            }
            Self::Fields {
                mut direct,
                mut fields,
            } => {
                while let Some(field) = fields.pop() {
                    if let Some(memory) = field.memory.and_then(Self::into_reservation) {
                        direct.absorb(memory);
                    }
                }
                // Constructor records are temporary; their capacity is not executable storage.
                drop(fields);
                Some(direct)
            }
        }
    }

    pub(super) fn field_mut(&mut self, name: &str) -> &mut Option<Self> {
        let Self::Fields { fields, .. } = self else {
            panic!("scalar ownership has no named plan fields");
        };
        &mut fields
            .iter_mut()
            .find(|field| field.name == name)
            .expect("copied plan field")
            .memory
    }

    pub(super) fn item_mut(&mut self, index: usize) -> &mut Option<Self> {
        let Self::Fields { fields, .. } = self else {
            panic!("scalar ownership has no plan items");
        };
        &mut fields[index].memory
    }

    pub(super) fn scalar_mut(&mut self) -> &mut Option<ScalarMemory> {
        match self {
            Self::Scalar(memory) => memory,
            Self::Fields { .. } => self
                .field_mut("scalar")
                .as_mut()
                .expect("scalar field")
                .scalar_mut(),
            Self::Scalars { .. } => panic!("scalar vector requires an item index"),
        }
    }

    pub(super) fn scalar_item_mut(&mut self, index: usize) -> &mut Option<ScalarMemory> {
        match self {
            Self::Scalars { items, .. } => &mut items[index],
            Self::Fields { .. } => self
                .item_mut(index)
                .as_mut()
                .expect("scalar item")
                .scalar_mut(),
            Self::Scalar(_) => panic!("scalar field has no indexed items"),
        }
    }
}
