//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Release displaced placeholder groups before their original declarations.

use super::{RoutineRegistry, SQLRoutine};
use crate::catalog::named_definitions::DefinitionEntry;
use uqa_core::memory::{MemoryBudget, MemoryError, MemoryReservation, RetainedMapRetirement};

pub struct RoutineRegistryRetirement {
    entries: RetainedMapRetirement<String, DefinitionEntry<Vec<SQLRoutine>>>,
    overloads: Option<Overloads>,
}

struct Overloads {
    values: std::vec::IntoIter<SQLRoutine>,
    _memory: MemoryReservation,
}

impl RoutineRegistry {
    /// Keep the installed roots live through their replacement. The finalization owns each original declaration until both roots have retired.
    pub fn prepare_placeholder_retirement(
        &self,
        memory: &MemoryBudget,
    ) -> Result<RoutineRegistryRetirement, MemoryError> {
        Ok(RoutineRegistryRetirement {
            entries: self.definitions.prepare_retirement(memory)?,
            overloads: None,
        })
    }
}

impl RoutineRegistryRetirement {
    /// Release at most one selected map node or overload reference. Independent readers retain their own generations.
    pub fn advance(&mut self) -> bool {
        if let Some(group) = &mut self.overloads {
            if group.values.next().is_none() {
                self.overloads = None;
            }
            return false;
        }
        let complete = self.entries.advance(|entry| {
            let ((_key, value), _entry_memory) = entry.into_parts();
            // Newly installed placeholders are native. The predecessor owns any inherited image entries.
            if let DefinitionEntry::Native(group) = value {
                let (values, memory) = group.into_parts();
                self.overloads = Some(Overloads {
                    values: values.into_iter(),
                    _memory: memory,
                });
            }
        });
        complete && self.overloads.is_none()
    }
}
