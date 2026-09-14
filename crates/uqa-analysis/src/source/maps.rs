//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared edit sequences copy only when a retained view still owns the preceding sequence.

use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

use super::edits::EditMap;
use crate::AnalysisResult;

#[derive(Debug)]
pub(super) struct EditMaps {
    entries: BudgetedVec<Arc<Budgeted<EditMap>>>,
    _payload: MemoryReservation,
}

impl EditMaps {
    pub fn budget(&self) -> &MemoryBudget {
        self.entries.budget()
    }

    pub fn push(
        maps: &mut Option<Arc<Self>>,
        map: Arc<Budgeted<EditMap>>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        poll()?;
        if let Some(existing) = maps.as_mut().and_then(Arc::get_mut) {
            if existing.entries.budget().shares_allowance(budget) {
                existing.entries.push(map)?;
                return Ok(());
            }
        }
        let payload = budget.reserve(std::mem::size_of::<Self>())?;
        let mut entries = BudgetedVec::new(budget);
        entries.reserve(maps.as_ref().map_or(0, |maps| maps.entries.len()) + 1)?;
        if let Some(previous) = maps {
            for (index, map) in previous.entries.iter().enumerate() {
                if index % 1024 == 0 {
                    poll()?;
                }
                entries.push(map.clone())?;
            }
        }
        entries.push(map)?;
        *maps = Some(Arc::new(Self {
            entries,
            _payload: payload,
        }));
        Ok(())
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Arc<Budgeted<EditMap>>> {
        self.entries.iter()
    }
}

impl PartialEq for EditMaps {
    fn eq(&self, other: &Self) -> bool {
        *self.entries == *other.entries
    }
}

impl Eq for EditMaps {}
