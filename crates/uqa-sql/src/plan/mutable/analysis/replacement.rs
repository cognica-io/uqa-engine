//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replaced payloads keep their original constructor leases until the existing retirement owner finishes.

use super::super::{memory::Folding, MutableQueryPlan, MutableSourcePlan, RewriteMemory};
use crate::{
    catalog::work::Work,
    plan::{
        retirement::construction::{Context, Prefix, Retirement, Root},
        QueryPlan, SourcePlan,
    },
    SQLError,
};
use uqa_core::memory::ProductionControl;

enum Storage<T: Root> {
    Ordinary(T),
    Retained(Prefix<T>),
}
impl<T: Root> Storage<T> {
    fn value(&self) -> &T {
        match self {
            Self::Ordinary(value) => value,
            Self::Retained(value) => value,
        }
    }

    fn value_mut(&mut self) -> &mut T {
        match self {
            Self::Ordinary(value) => value,
            Self::Retained(value) => value,
        }
    }
    fn take(self) -> T {
        match self {
            Self::Ordinary(value) => value,
            Self::Retained(value) => value.take(),
        }
    }
}

macro_rules! replacement {
    ($name:ident, $value:ty) => {
        pub(crate) struct $name(Storage<$value>);
        impl From<$value> for $name {
            fn from(value: $value) -> Self {
                Self(Storage::Ordinary(value))
            }
        }
        impl From<Prefix<$value>> for $name {
            fn from(value: Prefix<$value>) -> Self {
                Self(Storage::Retained(value))
            }
        }
    };
}
replacement!(QueryReplacement, MutableQueryPlan);
replacement!(SourceReplacement, MutableSourcePlan);

impl std::ops::Deref for QueryReplacement {
    type Target = QueryPlan;
    fn deref(&self) -> &QueryPlan {
        self.0.value()
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Scope<'a> {
    pub(super) context: Option<&'a Context>,
    pub(super) depth: usize,
}
impl Scope<'_> {
    fn prepare<T: Root>(
        self,
        replacement: &Storage<T>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Retirement>, SQLError> {
        let Some(context) = self.context else {
            return Ok(None);
        };
        let Storage::Retained(replacement) = replacement else {
            return Err(SQLError::Internal(
                "retained plan replacement requires its constructor retirement owner".into(),
            ));
        };
        context.admit_replacement(replacement.context(), self.depth, control)?;
        context.prepare_retirement(control).map(Some)
    }
}

pub(super) struct Discarded {
    // Payload cleanup must finish before the constructor records return their reservations, including cancellation and abandonment.
    tree: Retirement,
    memory: Option<RewriteMemory>,
}
impl Discarded {
    pub(super) async fn retire_from<W: Work>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.tree.retire_from(control, work).await?;
        if let Some(memory) = self.memory.take() {
            Folding::new(memory)
                .retire_from(Some(control), work)
                .await?;
        }
        Ok(())
    }
}

impl QueryReplacement {
    pub(crate) fn install_uncontrolled(self, query: &mut QueryPlan) -> Result<(), SQLError> {
        self.install(
            query,
            None,
            Scope::default(),
            &ProductionControl::uncontrolled(),
        )
        .map(drop)
    }

    pub(super) fn install(
        mut self,
        query: &mut QueryPlan,
        memory: Option<&mut RewriteMemory>,
        scope: Scope<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Discarded>, SQLError> {
        self.0.value_mut().assert_owner(control);
        control.check()?;
        let retirement = scope.prepare(&self.0, control)?;
        let replacement = self.0.value_mut();
        let replacement_memory = replacement
            .memory
            .as_mut()
            .map(|memory| memory.field_mut("root"));
        let memory = memory.map(|memory| memory.field_mut("root"));
        match (memory, replacement_memory) {
            (Some(memory), Some(replacement)) => std::mem::swap(memory, replacement),
            (None, None) => (),
            _ => unreachable!("query replacement preserves constructor ownership"),
        }
        std::mem::swap(&mut query.root, &mut replacement.query.root);
        let replacement = self.0.take();
        if let Some(mut tree) = retirement {
            tree.query(replacement.query);
            Ok(Some(Discarded {
                tree,
                memory: replacement.memory.into_parts(),
            }))
        } else {
            drop(replacement);
            Ok(None)
        }
    }
}

impl SourceReplacement {
    pub(super) fn install(
        mut self,
        source: &mut SourcePlan,
        memory: Option<&mut RewriteMemory>,
        scope: Scope<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Discarded>, SQLError> {
        self.0.value_mut().assert_owner(control);
        control.check()?;
        let retirement = scope.prepare(&self.0, control)?;
        let replacement = self.0.value_mut();
        match (memory, replacement.memory.as_mut()) {
            (Some(memory), Some(replacement)) => std::mem::swap(memory, replacement),
            (None, None) => (),
            _ => unreachable!("source replacement preserves constructor ownership"),
        }
        std::mem::swap(source, &mut replacement.source);
        let replacement = self.0.take();
        if let Some(mut tree) = retirement {
            tree.source(replacement.source);
            Ok(Some(Discarded {
                tree,
                memory: replacement.memory.into_parts(),
            }))
        } else {
            drop(replacement);
            Ok(None)
        }
    }
}
