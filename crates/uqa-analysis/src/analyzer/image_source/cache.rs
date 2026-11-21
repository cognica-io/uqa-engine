//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live publications of one revision or dictionary, grouped by construction allowance.

use parking_lot::Mutex;
use std::sync::{Arc, OnceLock, Weak};
use uqa_core::{
    memory::{MemoryBudget, MemoryBudgetIdentity, OwnedMap, Produced, ProductionControl},
    ValueRetentionError,
};

pub(super) type SharedImage<T> = Arc<Produced<Image<T>>>;
type Allowance = Option<MemoryBudgetIdentity>;
type Publications<T> = OwnedMap<Allowance, Weak<Produced<Image<T>>>>;
type Entries<T> = Mutex<Publications<T>>;

#[derive(Debug)]
struct Registration<T> {
    entries: Weak<Entries<T>>,
    allowance: Allowance,
}

impl<T> Drop for Registration<T> {
    fn drop(&mut self) {
        if let Some(entries) = self.entries.upgrade() {
            let mut entries = entries.lock();
            // A publisher can replace an expired weak pointer before this destructor takes the lock.
            if entries
                .get(&self.allowance)
                .is_some_and(|current| current.strong_count() == 0)
            {
                entries.remove(&self.allowance);
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct Image<T> {
    value: T,
    registration: OnceLock<Registration<T>>,
}

impl<T> Image<T> {
    pub(super) fn new(value: T) -> Self {
        Self {
            value,
            registration: OnceLock::new(),
        }
    }
}

impl<T> std::ops::Deref for Image<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

#[derive(Debug)]
pub(crate) struct ImageCache<T>(OnceLock<Arc<Entries<T>>>);

impl<T> Default for ImageCache<T> {
    fn default() -> Self {
        Self(OnceLock::new())
    }
}

impl<T> ImageCache<T> {
    pub(super) fn get_or_prepare(
        &self,
        control: &ProductionControl<'_>,
        prepare: impl FnOnce() -> Result<SharedImage<T>, ValueRetentionError>,
    ) -> Result<SharedImage<T>, ValueRetentionError> {
        control.check()?;
        let cache = self.0.get_or_init(Default::default);
        let mut entries = cache.lock();
        control.check()?;
        let allowance = control.budget().map(MemoryBudget::identity);
        if let Some(image) = entries.get(&allowance).and_then(Weak::upgrade) {
            return Ok(image);
        }
        Self::publish(cache, &mut entries, control.budget(), prepare()?)
    }

    pub(super) fn publish_prepared(
        &mut self,
        control: &ProductionControl<'_>,
        image: SharedImage<T>,
    ) -> Result<SharedImage<T>, ValueRetentionError> {
        control.check()?;
        let cache = self.0.get_or_init(Default::default);
        Self::publish(cache, &mut cache.lock(), control.budget(), image)
    }

    fn publish(
        cache: &Arc<Entries<T>>,
        entries: &mut Publications<T>,
        budget: Option<&MemoryBudget>,
        image: SharedImage<T>,
    ) -> Result<SharedImage<T>, ValueRetentionError> {
        let allowance = budget.map(MemoryBudget::identity);
        let weak = Arc::downgrade(&image);
        if let Some(budget) = budget {
            entries.insert_budgeted(allowance, weak, budget)?;
        } else {
            entries.insert(allowance, weak);
        }
        let registration = Registration {
            entries: Arc::downgrade(cache),
            allowance,
        };
        assert!(
            image.registration.set(registration).is_ok(),
            "new image already registered"
        );
        Ok(image)
    }
}
