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
    Scalar(ScalarMemory),
}

#[derive(Debug)]
pub(crate) struct RewriteField {
    pub(crate) name: &'static str,
    pub(crate) memory: Option<RewriteMemory>,
}

impl RewriteMemory {
    pub(super) fn reserved_bytes(&self) -> usize {
        match self {
            Self::Scalar(memory) => memory.reserved_bytes(),
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

    pub(crate) fn into_reservation(self) -> MemoryReservation {
        match self {
            Self::Scalar(memory) => memory.into_reservation(),
            Self::Fields {
                mut direct,
                mut fields,
            } => {
                while let Some(field) = fields.pop() {
                    if let Some(memory) = field.memory {
                        direct.absorb(memory.into_reservation());
                    }
                }
                // Constructor records are temporary; their capacity is not executable storage.
                drop(fields);
                direct
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

    pub(super) fn scalar_mut(&mut self) -> &mut Option<Self> {
        self.field_mut("scalar")
    }
}
