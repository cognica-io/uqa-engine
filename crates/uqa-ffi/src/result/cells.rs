//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Result cells have stable handles and occupy only pages that a caller has observed.

use crate::{boundary, value::UQAValue};
use uqa_core::memory::{BudgetedMap, BudgetedVec, MemoryBudget};

const CELLS_PER_PAGE: usize = 32;

struct Page {
    number: u128,
    slots: [Option<Box<UQAValue>>; CELLS_PER_PAGE],
}

impl Page {
    fn get_or_create(
        &mut self,
        offset: usize,
        create: impl FnOnce() -> boundary::Result<Box<UQAValue>>,
    ) -> boundary::Result<*const UQAValue> {
        let slot = &mut self.slots[offset];
        let value = match slot {
            Some(value) => value,
            None => slot.insert(create()?),
        };
        Ok(std::ptr::from_ref(&**value))
    }
}

pub(super) struct CellViews {
    pages: BudgetedVec<Page>,
    // Only pages first observed out of order need a directory entry.
    positions: BudgetedMap<u128, usize>,
}

impl CellViews {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            pages: BudgetedVec::new(memory),
            positions: BudgetedMap::new(memory),
        }
    }

    pub(super) fn get_or_create(
        &mut self,
        index: u128,
        create: impl FnOnce() -> boundary::Result<Box<UQAValue>>,
    ) -> boundary::Result<*const UQAValue> {
        let number = index / CELLS_PER_PAGE as u128;
        let offset = (index % CELLS_PER_PAGE as u128) as usize;
        let direct = usize::try_from(number)
            .ok()
            .filter(|&i| self.pages.get(i).is_some_and(|page| page.number == number));
        if let Some(position) = direct.or_else(|| self.positions.get(&number).copied()) {
            return self.pages[position].get_or_create(offset, create);
        }
        let mut page = Page {
            number,
            slots: std::array::from_fn(|_| None),
        };
        let pointer = page.get_or_create(offset, create)?;
        let position = self.pages.len();
        let entry = if number == position as u128 {
            None
        } else {
            Some(self.positions.prepare_entry(number, position)?)
        };
        self.pages.push(page)?;
        if let Some(entry) = entry {
            self.positions.insert_prepared(entry);
        }
        Ok(pointer)
    }
}
