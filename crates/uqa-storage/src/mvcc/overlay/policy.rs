//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private roots either summarize provider revisions or use an earlier resident-tier limit.

use uqa_core::memory::MemoryBudget;

use super::{PrivateRevisionScope, RevisionScopes};

pub(super) enum RootPolicy {
    Scoped(RevisionScopes),
    Unscoped(Option<usize>),
}

impl RootPolicy {
    pub(super) fn new(memory: &MemoryBudget, scope: Option<PrivateRevisionScope>) -> Self {
        match scope {
            Some(scope) => Self::Scoped(RevisionScopes::new(memory, scope)),
            None => Self::Unscoped(None),
        }
    }

    pub(super) fn memory_tier_limit(&self) -> Option<usize> {
        match self {
            Self::Scoped(_) => None,
            Self::Unscoped(limit) => *limit,
        }
    }

    pub(super) fn scopes(&self) -> Option<&RevisionScopes> {
        match self {
            Self::Scoped(scopes) => Some(scopes),
            Self::Unscoped(_) => None,
        }
    }

    pub(super) fn fork(&self) -> Self {
        match self {
            Self::Scoped(scopes) => Self::Scoped(scopes.fork()),
            Self::Unscoped(limit) => Self::Unscoped(*limit),
        }
    }

    pub(super) fn take_scopes(&mut self) -> Option<RevisionScopes> {
        if matches!(self, Self::Unscoped(_)) {
            return None;
        }
        match std::mem::replace(self, Self::Unscoped(None)) {
            Self::Scoped(scopes) => Some(scopes),
            Self::Unscoped(_) => unreachable!("checked scoped root policy"),
        }
    }

    /// Undo changes revision summaries but never changes an unscoped owner's spill policy.
    pub(super) fn restore_scopes(&mut self, scopes: Option<RevisionScopes>) {
        match scopes {
            Some(scopes) => *self = Self::Scoped(scopes),
            None if matches!(self, Self::Scoped(_)) => *self = Self::Unscoped(None),
            None => {}
        }
    }
}
