//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable JSON containers own their capacity independently of each node's payload.

use crate::error::Result;
use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_core::{memory::ProductionControl, ValueRetentionError};

pub(super) enum Values<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

impl<T> Values<T> {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        control.budget().map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Controlled(BudgetedVec::new(budget)),
        )
    }

    pub(super) fn push(
        &mut self,
        value: T,
        control: &ProductionControl<'_>,
    ) -> std::result::Result<(), ValueRetentionError> {
        control.check()?;
        match self {
            Self::Ordinary(values) => values.push(value),
            Self::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }

    pub(super) fn insert(
        &mut self,
        index: usize,
        value: T,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        self.push(value, control)?;
        for index in (index + 1..self.len()).rev() {
            control.check()?;
            self.as_mut_slice().swap(index, index - 1);
        }
        Ok(())
    }

    pub(super) fn remove(&mut self, index: usize, control: &ProductionControl<'_>) -> Result<()> {
        for index in index..self.len() - 1 {
            control.check()?;
            self.as_mut_slice().swap(index, index + 1);
        }
        match self {
            Self::Ordinary(values) => {
                values.pop();
            }
            Self::Controlled(values) => {
                values.pop();
            }
        }
        Ok(())
    }

    pub(super) fn retain(
        &mut self,
        control: &ProductionControl<'_>,
        mut keep: impl FnMut(&T) -> Result<bool>,
    ) -> Result<()> {
        let mut retained = 0;
        for index in 0..self.len() {
            control.check()?;
            if keep(&self[index])? {
                self.as_mut_slice().swap(retained, index);
                retained += 1;
            }
        }
        match self {
            Self::Ordinary(values) => values.truncate(retained),
            Self::Controlled(values) => values.truncate(retained),
        }
        Ok(())
    }

    pub(super) fn truncate(&mut self, length: usize) {
        match self {
            Self::Ordinary(values) => values.truncate(length),
            Self::Controlled(values) => values.truncate(length),
        }
    }

    pub(super) fn as_mut_slice(&mut self) -> &mut [T] {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }

    pub(super) fn into_owned_iter(self) -> OwnedIter<T> {
        let (values, memory) = match self {
            Self::Ordinary(values) => (values, None),
            Self::Controlled(values) => {
                let (values, memory) = values.into_parts();
                (values, Some(memory))
            }
        };
        OwnedIter {
            values: values.into_iter(),
            _memory: memory,
        }
    }
}

impl<T> std::ops::Deref for Values<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }
}

// Remaining elements and buffer are freed before their capacity reservation on every exit.
pub(super) struct OwnedIter<T> {
    values: std::vec::IntoIter<T>,
    _memory: Option<MemoryReservation>,
}
impl<T> Iterator for OwnedIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.values.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    struct Witness<'a> {
        budget: &'a MemoryBudget,
        drops: &'a Cell<usize>,
    }
    impl Drop for Witness<'_> {
        fn drop(&mut self) {
            assert!(
                self.budget.used() > 0,
                "container lease must outlive its elements"
            );
            self.drops.set(self.drops.get() + 1);
        }
    }

    #[test]
    fn json_container_iteration_retains_capacity_through_failure_and_unwind() {
        for unwind in [false, true] {
            let memory = MemoryBudget::new(1024);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let drops = Cell::new(0);
            let make = || {
                let mut values = Values::new(&control);
                values
                    .push(
                        Witness {
                            budget: &memory,
                            drops: &drops,
                        },
                        &control,
                    )
                    .unwrap();
                values
                    .push(
                        Witness {
                            budget: &memory,
                            drops: &drops,
                        },
                        &control,
                    )
                    .unwrap();
                values.into_owned_iter()
            };
            if unwind {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut values = make();
                    drop(values.next());
                    panic!("injected JSON container failure");
                }));
                assert!(result.is_err());
            } else {
                let result = (|| -> std::result::Result<(), ()> {
                    let mut values = make();
                    drop(values.next());
                    Err(())?;
                    Ok(())
                })();
                assert!(result.is_err());
            }
            assert_eq!(drops.get(), 2);
            assert_eq!(memory.used(), 0);
        }
    }
}
