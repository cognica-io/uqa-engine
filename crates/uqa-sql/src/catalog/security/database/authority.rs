//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable database authority retains one complete admitted ACL generation.

use super::BoundDatabaseSecurity;
use crate::SQLError;
use serde::{Serialize, Serializer};
use std::{ops::Deref, sync::Arc};
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};

#[derive(Debug, Clone)]
pub struct DatabaseAuthority {
    value: Arc<Produced<BoundDatabaseSecurity>>,
    memory: MemoryBudget,
}

impl DatabaseAuthority {
    pub fn bootstrap() -> Self {
        Self::with_budget(
            &BoundDatabaseSecurity::bootstrap(),
            &MemoryBudget::new(usize::MAX),
        )
        .expect("fixed bootstrap authority fits the native allowance")
    }

    /// Admit the complete tuple before publishing its immutable generation.
    pub fn with_budget(
        value: &BoundDatabaseSecurity,
        memory: &MemoryBudget,
    ) -> Result<Self, SQLError> {
        let token = CancellationToken::new();
        let control = ProductionControl::new(memory, &token, &token);
        let value = value.capture_security(&control)?.into_shared(&control)?;
        Ok(Self {
            value,
            memory: memory.clone(),
        })
    }

    /// Unchanged authority shares its generation; a changed successor is fully funded before the caller can install it.
    pub fn successor(&self, value: &BoundDatabaseSecurity) -> Result<Self, SQLError> {
        if self.as_ref() == value {
            return Ok(self.clone());
        }
        Self::with_budget(value, &self.memory)
    }

    /// Capture charges the cached complete footprint without copying or walking the ACL.
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedDatabaseAuthority>, SQLError> {
        control.check()?;
        let memory = control.budget().ok_or_else(|| {
            SQLError::Internal("database authority capture requires an admitted control".into())
        })?;
        let lease = memory.reserve(self.value.reserved_bytes())?;
        Ok(control.finish(
            CapturedDatabaseAuthority {
                value: self.value.clone(),
            },
            Some(lease),
        )?)
    }
}

impl AsRef<BoundDatabaseSecurity> for DatabaseAuthority {
    fn as_ref(&self) -> &BoundDatabaseSecurity {
        &self.value
    }
}

impl Deref for DatabaseAuthority {
    type Target = BoundDatabaseSecurity;
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl PartialEq for DatabaseAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for DatabaseAuthority {}

impl Serialize for DatabaseAuthority {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_ref().serialize(serializer)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedDatabaseAuthority {
    value: Arc<Produced<BoundDatabaseSecurity>>,
}

impl AsRef<BoundDatabaseSecurity> for CapturedDatabaseAuthority {
    fn as_ref(&self) -> &BoundDatabaseSecurity {
        &self.value
    }
}

impl Deref for CapturedDatabaseAuthority {
    type Target = BoundDatabaseSecurity;
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl Serialize for CapturedDatabaseAuthority {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_ref().serialize(serializer)
    }
}

#[cfg(test)]
mod tests;
