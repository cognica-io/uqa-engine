//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable authority references retain their producer's private name through every scope clone.

use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{catalog::roles::RoleReference, SQLError};

#[derive(Clone)]
pub(in crate::query::scope) enum ScopeSubject {
    Ordinary(RoleReference),
    Captured(Arc<Produced<RoleReference>>),
}

impl ScopeSubject {
    pub(in crate::query::scope) fn capture(
        subject: &RoleReference,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::from_produced(subject.clone_with_control(control)?, control)
    }

    pub(in crate::query::scope) fn from_produced(
        subject: Produced<RoleReference>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self::Captured(subject.into_shared(control)?))
    }
}

impl From<RoleReference> for ScopeSubject {
    fn from(subject: RoleReference) -> Self {
        Self::Ordinary(subject)
    }
}

#[cfg(test)]
impl From<&str> for ScopeSubject {
    fn from(subject: &str) -> Self {
        Self::Ordinary(subject.into())
    }
}

impl std::ops::Deref for ScopeSubject {
    type Target = RoleReference;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Ordinary(subject) => subject,
            Self::Captured(subject) => subject,
        }
    }
}

#[cfg(test)]
mod tests;
