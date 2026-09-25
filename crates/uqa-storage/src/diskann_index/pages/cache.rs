//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::{
    Budgeted, BudgetedDeque, BudgetedMap, BudgetedVec, MemoryBudget, MemoryError,
};

use super::copy;
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

pub(super) type SharedPage = Arc<Budgeted<BudgetedVec<u8>>>;

struct Entries {
    pages: BudgetedMap<u64, SharedPage>,
    order: BudgetedDeque<u64>,
}

pub(super) struct PageCache {
    entries: Mutex<Entries>,
    budget: MemoryBudget,
}

impl PageCache {
    pub(super) fn new(parent: &MemoryBudget, limit: usize) -> Self {
        let budget = parent.child(limit);
        Self {
            entries: Mutex::new(Entries {
                pages: BudgetedMap::new(&budget),
                order: BudgetedDeque::new(&budget),
            }),
            budget,
        }
    }

    pub(super) fn get(&self, page: u64) -> Option<SharedPage> {
        self.entries.lock().pages.get(&page).cloned()
    }

    pub(super) fn admit(
        &self,
        id: u64,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<SharedPage>> {
        control.check()?;
        let mut entries = self.entries.lock();
        if let Some(page) = entries.pages.get(&id) {
            return Ok(Some(Arc::clone(page)));
        }
        let owner = StorageReadControl::new(&self.budget, control.cancellation());
        loop {
            control.check()?;
            let prepare = || -> StorageBackendResult<_> {
                let bytes = copy(bytes, self.budget.limit(), &owner)?;
                let page = Budgeted::new(bytes, self.budget.empty_reservation()).into_shared()?;
                let entry = entries.pages.prepare_entry(id, Arc::clone(&page))?;
                Ok((page, entry))
            };
            let prepared = prepare().and_then(|pair| {
                entries.order.reserve(1)?;
                Ok(pair)
            });
            match prepared {
                Ok((page, entry)) => {
                    entries.pages.insert_prepared(entry);
                    entries
                        .order
                        .push_back(id)
                        .expect("reserved cache order entry");
                    return Ok(Some(page));
                }
                Err(StorageBackendError::Memory(
                    MemoryError::Limit { .. }
                    | MemoryError::Allocation(_)
                    | MemoryError::SizeOverflow,
                )) => {
                    let Some(oldest) = entries.order.pop_front() else {
                        return Ok(None);
                    };
                    entries.pages.remove(&oldest);
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn used(&self) -> usize {
        self.budget.used()
    }
}
