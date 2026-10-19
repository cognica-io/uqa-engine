//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned compiled patterns. Matching never holds the cache lock or borrows another invocation's cancellation or scratch allowance.

use std::{
    cell::RefCell,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Mutex},
};
use uqa_core::memory::{BudgetedString, BudgetedVec, MemoryBudget, MemoryError, MemoryReservation};
use uqa_pg_regex::{CompileFlags, RegexResources, RetainedRegex};

const CAPACITY: usize = 32;

pub(super) struct RetainedAllowance(pub(super) Option<MemoryReservation>);

impl RegexResources for RetainedAllowance {
    fn reserve(&mut self, _: usize) -> bool {
        unreachable!("retained hooks only release compiled memory")
    }
    fn release(&mut self, bytes: usize) {
        if let Some(lease) = self.0.as_mut() {
            drop(lease.split(bytes));
        }
    }
    fn cancel_requested(&mut self) -> bool {
        false
    }
    fn stack_too_deep(&mut self, _: usize) -> bool {
        false
    }
}

pub(super) struct Entry {
    pub(super) pattern: BudgetedString,
    pub(super) flags: CompileFlags,
    pub(super) collation: u32,
    pub(super) generation: u64,
    pub(super) regex: RetainedRegex<RetainedAllowance>,
}

pub(super) struct Admission {
    pub(super) pattern: BudgetedString,
    pub(super) generation: u64,
    pub(super) memory: Option<MemoryReservation>,
}

impl Entry {
    fn matches(&self, pattern: &str, flags: CompileFlags, collation: u32) -> bool {
        &*self.pattern == pattern && self.flags == flags && self.collation == collation
    }
}

#[derive(Default)]
struct Entries {
    values: Vec<Entry>,
    memory: Option<MemoryReservation>,
    generation: u64,
}

/// A logical session's bounded regular-expression cache. The supplied allowance owns pattern keys, cache slots and compiled contexts. Active calls temporarily remove their entry so independent workers never serialize matching on this lock. In-flight entries remain charged; at most 32 completed patterns are retained.
pub struct RegexCache {
    pub(super) budget: MemoryBudget,
    entries: Mutex<Entries>,
}

impl RegexCache {
    pub fn new(budget: MemoryBudget) -> Self {
        Self {
            budget,
            entries: Mutex::new(Entries::default()),
        }
    }

    /// Whether this cache's allocations belong to the supplied retention owner.
    pub fn shares_allowance(&self, budget: &MemoryBudget) -> bool {
        self.budget.shares_allowance(budget)
    }

    /// Release all idle patterns and their index allocation. Active invocations retain their owned state until they finish, then discard it if still cleared.
    pub fn clear(&self) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *entries = Entries {
            generation: entries.generation.wrapping_add(1),
            ..Entries::default()
        };
    }

    pub(super) fn take(&self, pattern: &str, flags: CompileFlags, collation: u32) -> Option<Entry> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let index = entries
            .values
            .iter()
            .position(|entry| entry.matches(pattern, flags, collation))?;
        Some(entries.values.remove(index))
    }

    pub(super) fn generation(&self) -> u64 {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .generation
    }

    /// Retain only completed compiled state. Refused retention leaves the ordinary invocation usable with its original resource contract.
    pub(super) fn admit(
        &self,
        pattern: &str,
        bytes: usize,
        original: Option<&MemoryBudget>,
        generation: u64,
    ) -> Option<Admission> {
        if self.prepare().ok()? != generation {
            return None;
        }
        let shared = original.is_some_and(|budget| self.budget.shares_allowance(budget));
        let additional_state = if shared { 0 } else { bytes };
        let mut key = BudgetedString::new(&self.budget);
        loop {
            match key.push_str(pattern) {
                Ok(()) => break,
                Err(ref error) if self.evict_for(error, additional_state) => {}
                Err(_) => return None,
            }
        }
        let memory = if shared {
            None
        } else {
            loop {
                match self.budget.reserve(bytes) {
                    Ok(memory) => break Some(memory),
                    Err(ref error) if self.evict_for(error, 0) => {}
                    Err(_) => return None,
                }
            }
        };
        Some(Admission {
            pattern: key,
            generation,
            memory,
        })
    }

    pub(super) fn prepare(&self) -> Result<u64, MemoryError> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries.values.capacity() == 0 {
            let mut slots = BudgetedVec::<Entry>::new(&self.budget);
            slots.reserve(CAPACITY)?;
            let (values, memory) = slots.into_parts();
            *entries = Entries {
                values,
                memory: Some(memory),
                generation: entries.generation,
            };
        }
        Ok(entries.generation)
    }

    fn evict_for(&self, error: &MemoryError, pending_bytes: usize) -> bool {
        let MemoryError::Limit { required, limit } = error else {
            return false;
        };
        let Some(missing) = required
            .checked_sub(*limit)
            .and_then(|bytes| bytes.checked_add(pending_bytes))
        else {
            return false;
        };
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // The refusal includes the actual key allocation and any limiting ancestor. Both the key and still-unadmitted compiled state must fit after reclaiming idle entries; active calls and unrelated retained owners cannot be evicted.
        let reclaimable = entries.values.iter().try_fold(0usize, |bytes, entry| {
            bytes
                .checked_add(entry.pattern.capacity())?
                .checked_add(entry.regex.reserved_bytes())
        });
        if entries.values.is_empty() || reclaimable.is_none_or(|bytes| bytes < missing) {
            return false;
        }
        entries.values.remove(0);
        true
    }

    pub(super) fn keep(&self, entry: Entry) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entry.generation != entries.generation
            || entries.memory.is_none()
            || entries
                .values
                .iter()
                .any(|old| old.matches(&entry.pattern, entry.flags, entry.collation))
        {
            return;
        }
        if entries.values.len() == CAPACITY {
            entries.values.remove(0);
        }
        // prepare admitted a full-capacity buffer before compilation; returning an invocation never allocates, including during unwinding or cancellation.
        entries.values.push(entry);
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<RegexCache>>> = const { RefCell::new(None) };
}

/// Capture the current session cache for a worker. Outside a session, no cache survives the call and the ordinary resource contract is unchanged.
pub fn current_regex_cache() -> Option<Arc<RegexCache>> {
    CURRENT.with_borrow(Clone::clone)
}

/// Enter a session's cache on this thread, restoring its caller on every exit.
pub struct RegexCacheScope {
    previous: Option<Arc<RegexCache>>,
    thread: PhantomData<Rc<()>>,
}

impl RegexCacheScope {
    pub fn enter(cache: Option<Arc<RegexCache>>) -> Self {
        Self {
            previous: CURRENT.replace(cache),
            thread: PhantomData,
        }
    }
}

impl Drop for RegexCacheScope {
    fn drop(&mut self) {
        CURRENT.set(self.previous.take());
    }
}

#[cfg(test)]
pub(super) mod tests;
