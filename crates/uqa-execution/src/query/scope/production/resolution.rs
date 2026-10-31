//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    catalog::resolution::{RelationLookupMode, RelationNameResolution},
    SQLError,
};

#[derive(Clone)]
pub(in crate::query::scope) enum ScopeResolution {
    Ordinary(RelationNameResolution),
    Captured {
        namespace: Arc<Produced<RelationNameResolution>>,
        mode: RelationLookupMode,
    },
}

impl ScopeResolution {
    pub(super) fn capture(
        value: Produced<RelationNameResolution>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mode = value.lookup_mode;
        Ok(Self::Captured {
            namespace: value.into_shared(control)?,
            mode,
        })
    }

    pub(in crate::query::scope) fn retain_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        match self {
            Self::Captured { .. } => Ok(self.clone()),
            Self::Ordinary(_) => Self::capture(self.copy_with_control(control)?, control),
        }
    }

    pub(in crate::query::scope) fn snapshot(&self) -> RelationNameResolution {
        let mut value = (**self).clone();
        value.set_lookup_mode(self.lookup_mode());
        value
    }

    pub(super) fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationNameResolution>, SQLError> {
        let (mut value, memory) = (**self).copy_with_control(control)?.into_parts();
        value.set_lookup_mode(self.lookup_mode());
        Ok(control.finish(value, memory)?)
    }

    pub(in crate::query::scope) fn lookup_mode(&self) -> RelationLookupMode {
        match self {
            Self::Ordinary(value) => value.lookup_mode,
            Self::Captured { mode, .. } => *mode,
        }
    }

    pub(in crate::query::scope) fn set_lookup_mode(
        &mut self,
        mode: RelationLookupMode,
    ) -> RelationLookupMode {
        match self {
            Self::Ordinary(value) => value.set_lookup_mode(mode),
            Self::Captured { mode: selected, .. } => std::mem::replace(selected, mode),
        }
    }

    #[cfg(test)]
    pub(in crate::query::scope) fn ordinary_mut(&mut self) -> &mut RelationNameResolution {
        let Self::Ordinary(value) = self else {
            panic!("fixture requires an ordinary namespace")
        };
        value
    }
}

impl From<RelationNameResolution> for ScopeResolution {
    fn from(value: RelationNameResolution) -> Self {
        Self::Ordinary(value)
    }
}

impl std::ops::Deref for ScopeResolution {
    type Target = RelationNameResolution;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Ordinary(value) => value,
            Self::Captured { namespace, .. } => namespace,
        }
    }
}
