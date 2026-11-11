//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical handles share an admitted name without detaching its original allocation.

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use std::{fmt, ops::Deref, sync::Arc};
use uqa_core::memory::{Budgeted, MemoryError};

/// The immutable physical name used by document and index handles. Ordinary constructors keep their existing owned string; restored handles share the original admitted string.
#[derive(Clone, Debug)]
pub struct StorageName(Owner);

#[derive(Clone, Debug)]
enum Owner {
    Native(String),
    Admitted(Arc<Budgeted<String>>),
}

impl StorageName {
    /// Admit the shared header before allocation, preserving the input buffer and its original allowance. No input cancellation token is retained.
    pub fn from_admitted(value: Budgeted<String>) -> Result<Self, MemoryError> {
        Ok(Self(Owner::Admitted(value.into_shared()?)))
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            Owner::Native(value) => value,
            Owner::Admitted(value) => value,
        }
    }

    /// A controlled factory admits only a name owned by that same restore allowance.
    pub fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        control.check()?;
        match &self.0 {
            Owner::Admitted(value) if value.budget().shares_allowance(control.memory()) => Ok(()),
            _ => Err(StorageBackendError::Other(
                "controlled storage handles require their original admitted name".into(),
            )),
        }
    }
}

impl From<String> for StorageName {
    fn from(value: String) -> Self {
        Self(Owner::Native(value))
    }
}

impl From<&str> for StorageName {
    fn from(value: &str) -> Self {
        Self::from(value.to_owned())
    }
}

impl Deref for StorageName {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for StorageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}
