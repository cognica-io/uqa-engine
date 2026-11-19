//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each lexical scope keeps its original local name map and shares its inherited scope without copying names.

use crate::{binding::production::BindingMap, catalog::work::Work, plan::CtePlan, SQLError};
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

struct Names {
    local: BindingMap<usize>,
    parent: Scope,
}

#[derive(Clone, Default)]
pub(in crate::plan::mutable) struct Scope {
    names: Option<Arc<Produced<Names>>>,
    limit: usize,
    pub(super) external: bool,
}

impl Scope {
    pub(super) fn root() -> Self {
        Self {
            names: None,
            limit: 0,
            external: true,
        }
    }

    pub(super) fn visible(&self, name: &str) -> bool {
        let mut scope = self;
        while let Some(names) = &scope.names {
            if names
                .local
                .get(name)
                .is_some_and(|position| *position < scope.limit)
            {
                return true;
            }
            scope = &names.parent;
        }
        false
    }

    pub(in crate::plan::mutable) async fn contains<W: Work + Send>(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<bool, SQLError> {
        let mut scope = self;
        while let Some(names) = &scope.names {
            if names
                .local
                .get_from(name, control, work)
                .await?
                .is_some_and(|position| *position < scope.limit)
            {
                return Ok(true);
            }
            scope = &names.parent;
        }
        Ok(false)
    }

    pub(super) fn before(&self, index: usize, recursive: bool) -> Self {
        Self {
            names: self.names.clone(),
            limit: if recursive { usize::MAX } else { index + 1 },
            external: self.external,
        }
    }

    pub(super) async fn extend<W: Work + Send>(
        &self,
        ctes: &[CtePlan],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(Self, bool), SQLError> {
        if ctes.is_empty() {
            return Ok((self.clone(), false));
        }
        let mut local = BindingMap::new();
        let mut recursive = false;
        for (position, cte) in ctes.iter().enumerate() {
            control.check()?;
            work.checkpoint().await?;
            recursive |= cte.recursive;
            if local.get_from(&cte.name, control, work).await?.is_none() {
                local
                    .insert_from(&cte.name, position + 1, control, work)
                    .await?;
            }
        }
        let names = Names {
            local,
            parent: self.clone(),
        };
        let names = control
            .finish(names, control.empty_reservation())?
            .into_shared(control)?;
        Ok((
            Self {
                names: Some(names),
                limit: usize::MAX,
                external: self.external,
            },
            recursive,
        ))
    }

    pub(super) async fn retire<W: Work + Send>(
        mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        while let Some(names) = self.names.take().and_then(Arc::into_inner) {
            let (Names { local, mut parent }, owner) = names.into_parts();
            local.retire_from(control, work).await?;
            self.names = parent.names.take();
            drop(owner);
        }
        Ok(())
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        // A suspended child can be the final owner of an arbitrarily deep inherited chain.
        while let Some(names) = self.names.take().and_then(Arc::into_inner) {
            let (mut names, owner) = names.into_parts();
            self.names = names.parent.names.take();
            drop(names);
            drop(owner);
        }
    }
}
