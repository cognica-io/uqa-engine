//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constructor records for independently replaceable plan fields.

use super::super::scalar::ScalarMemory;
use uqa_core::memory::{BudgetedVec, MemoryReservation};

mod footprint;
mod retirement;
pub(super) use footprint::{RewriteFields, RootMemory};
pub(super) use retirement::Folding;

#[derive(Debug)]
pub(crate) enum RewriteMemory {
    Fields {
        // The direct reservation includes this field buffer. Drop its storage before releasing the lease.
        fields: RewriteFields,
        direct: MemoryReservation,
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
    pub(crate) fn fields(mut direct: MemoryReservation, fields: BudgetedVec<RewriteField>) -> Self {
        let (fields, buffer) = fields.into_parts();
        direct.absorb(buffer);
        Self::Fields {
            fields: RewriteFields::new(fields),
            direct,
        }
    }

    pub(super) fn direct(direct: MemoryReservation) -> Self {
        let fields = BudgetedVec::new(direct.budget());
        Self::fields(direct, fields)
    }

    pub(super) fn reserved_bytes(&mut self) -> usize {
        if let Self::Fields { direct, fields } = self {
            direct.bytes() + fields.child_bytes()
        } else {
            self.known_bytes().expect("scalar footprint")
        }
    }

    fn known_bytes(&self) -> Option<usize> {
        Some(match self {
            Self::Fields { direct, fields } => direct.bytes() + fields.known_child_bytes()?,
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
        })
    }

    pub(crate) fn into_reservation(self) -> Option<MemoryReservation> {
        retirement::fold(self)
    }

    pub(super) fn field_mut(&mut self, name: &str) -> &mut Option<Self> {
        let Self::Fields { fields, .. } = self else {
            panic!("scalar ownership has no named plan fields");
        };
        let index = fields
            .iter()
            .position(|field| field.name == name)
            .expect("copied plan field");
        &mut fields.edit_item(index).memory
    }

    /// Lend disjoint constructor fields together while keeping their payloads and reservations in the original owner.
    pub(super) fn fields_mut<const N: usize>(
        &mut self,
        names: [&str; N],
    ) -> [&mut Option<Self>; N] {
        let Self::Fields { fields, .. } = self else {
            panic!("scalar ownership has no named plan fields");
        };
        let mut selected = std::array::from_fn::<_, N, _>(|_| None);
        for field in fields.iter_mut() {
            if let Some(index) = names.iter().position(|name| *name == field.name) {
                assert!(selected[index].is_none(), "unique constructor field");
                selected[index] = Some(&mut field.memory);
            }
        }
        selected.map(|field| field.expect("copied plan field"))
    }

    pub(super) fn item_mut(&mut self, index: usize) -> &mut Option<Self> {
        let Self::Fields { fields, .. } = self else {
            panic!("scalar ownership has no plan items");
        };
        &mut fields.edit_item(index).memory
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
