//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared scan names carry their admission through row, alias and lock-candidate lifetimes.

use std::{
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::Arc,
};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::SQLError;

#[derive(Clone)]
pub struct RowLockName(Name);

#[derive(Clone)]
enum Name {
    Ordinary(Arc<str>),
    Admitted(Arc<Produced<String>>),
}

impl RowLockName {
    pub(crate) fn copy_with_control(
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if control.budget().is_none() {
            return Ok(Self::from(name));
        }
        Self::from_produced(control.copy_text(name)?, control)
    }

    pub(crate) fn from_produced(
        name: Produced<String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        if control.budget().is_some() {
            Ok(Self(Name::Admitted(name.into_shared(control)?)))
        } else {
            Ok(Self::from(
                name.into_uncontrolled().expect("ordinary row-lock name"),
            ))
        }
    }
}
impl Deref for RowLockName {
    type Target = str;
    fn deref(&self) -> &str {
        match &self.0 {
            Name::Ordinary(name) => name,
            Name::Admitted(name) => name.as_str(),
        }
    }
}
impl AsRef<str> for RowLockName {
    fn as_ref(&self) -> &str {
        self
    }
}
impl std::borrow::Borrow<str> for RowLockName {
    fn borrow(&self) -> &str {
        self
    }
}
impl From<Arc<str>> for RowLockName {
    fn from(name: Arc<str>) -> Self {
        Self(Name::Ordinary(name))
    }
}
impl From<String> for RowLockName {
    fn from(name: String) -> Self {
        Self::from(Arc::<str>::from(name))
    }
}
impl From<&str> for RowLockName {
    fn from(name: &str) -> Self {
        Self::from(Arc::<str>::from(name))
    }
}
impl PartialEq for RowLockName {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for RowLockName {}
impl PartialOrd for RowLockName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RowLockName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_ref().cmp(other.as_ref())
    }
}
impl Hash for RowLockName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_ref().hash(state);
    }
}
impl fmt::Debug for RowLockName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_ref(), f)
    }
}
impl fmt::Display for RowLockName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_ref(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn scan_lock_names_follow_the_last_row_and_rebound_alias() {
        let text = "q".repeat(1 << 20);
        let memory = MemoryBudget::new(8 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let occupied = memory.reserve(memory.available() - 1024).unwrap();
        let mut denied = None;
        let allocations = allocation_counter::measure(|| {
            denied = Some(RowLockName::copy_with_control(&text, &control));
        });
        assert_eq!(denied.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert!(allocations.bytes_total < 4096, "{allocations:?}");
        assert_eq!(memory.used(), occupied.bytes());
        drop(occupied);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                RowLockName::copy_with_control(&text, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), 0);
            token.reset();
        }
        let qualifier = RowLockName::copy_with_control(&text, &control).unwrap();
        let storage = RowLockName::copy_with_control("public.t", &control).unwrap();
        let origin = crate::RowLockOrigin::from_shared(qualifier.clone(), storage, 7);
        let row = crate::PhysicalRow::from_values(Vec::new()).with_lock_origin(origin);
        let before = memory.used();
        drop(qualifier);
        assert_eq!(memory.used(), before);
        let retained = row.clone().rebind_lock_origin_qualifiers("alias");
        drop(row);
        assert_eq!(memory.used(), before);
        let retained_origin = retained.lock_origins()[0].clone();
        assert_eq!(retained_origin.qualifier.as_ref(), "alias");
        assert_eq!(retained_origin.scan_qualifier.as_ref(), text);
        assert_eq!(retained_origin.storage_name.as_ref(), "public.t");
        drop(retained);
        assert_eq!(memory.used(), before);
        drop(retained_origin);
        assert_eq!(memory.used(), 0);
    }
}
