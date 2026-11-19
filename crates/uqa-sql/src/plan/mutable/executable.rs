//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable statement roots publish only after their independent child edits finish.

use super::{
    edits::Editable, memory::Folding, MutableCommandPlan, MutableQueryPlan, RewriteMemory,
};
use crate::{
    catalog::work::Work,
    plan::{
        retirement::{
            construction::{CompletedTree, Context, Prefix},
            Node,
        },
        CommandPlan, ExecutableMemory, OwnedExecutablePlan, PlanRef, QueryPlan, UnifiedPlan,
    },
    SQLError,
};
use uqa_core::memory::{MemoryReservation, ProductionControl};

#[derive(Debug)]
pub enum MutableExecutablePlan {
    Query(MutableQueryPlan),
    Command(MutableCommandPlan),
}

pub(in crate::plan) struct CompletedStatement {
    // Cancellation and abandonment retire native fields before their allocation leases.
    tree: CompletedTree,
    memory: Option<RewriteMemory>,
}

impl CompletedStatement {
    pub(in crate::plan) async fn retire_from<W: Work + Send>(
        mut self,
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

impl UnifiedPlan {
    pub fn copy_for_rewrite(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExecutablePlan, SQLError> {
        PlanRef::from(self).copy_for_rewrite(control)
    }
}

impl PlanRef<'_> {
    pub fn copy_for_rewrite(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExecutablePlan, SQLError> {
        Ok(match self {
            Self::Query(query) => MutableExecutablePlan::Query(query.copy_for_rewrite(control)?),
            Self::Command(command) => {
                MutableExecutablePlan::Command(command.copy_for_rewrite(control)?)
            }
        })
    }
}

impl MutableExecutablePlan {
    pub(in crate::plan) fn into_retirement(self, context: Context) -> CompletedStatement {
        let (node, memory) = match self {
            Self::Query(query) => {
                let (query, memory) = query.into_parts();
                (Node::Query(query), memory)
            }
            Self::Command(command) => {
                let (command, memory) = command.into_parts();
                (Node::Command(command), memory)
            }
        };
        CompletedStatement {
            tree: context.completed_tree(node),
            memory,
        }
    }

    /// Transfer a retained native executable through another entry point using the same allowance and its current cancellation tokens.
    pub fn transfer(self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        match &self {
            Self::Query(query) => query.assert_owner(control),
            Self::Command(command) => command.assert_owner(control),
        }
        control.check()?;
        Ok(self)
    }

    pub fn as_plan_ref(&self) -> PlanRef<'_> {
        match self {
            Self::Query(query) => PlanRef::Query(query),
            Self::Command(command) => PlanRef::Command(command),
        }
    }

    pub fn reserved_bytes(&self) -> usize {
        match self {
            Self::Query(query) => query.reserved_bytes(),
            Self::Command(command) => command.reserved_bytes(),
        }
    }

    /// Admit the final statement box and transfer only surviving constructor leases into immutable execution storage.
    pub fn finish(self, control: &ProductionControl<'_>) -> Result<OwnedExecutablePlan, SQLError> {
        let mut publication = Publication::new(self, |plan| plan, control)?;
        if let Some(fields) = &mut publication.fields {
            fields.drain();
        }
        let (plan, memory) = publication.finish(control)?;
        Ok(plan.boxed(memory))
    }

    fn boxed(self, memory: ExecutableMemory) -> OwnedExecutablePlan {
        let value = match self {
            Self::Query(query) => UnifiedPlan::Query(Box::new(query.query)),
            Self::Command(command) => UnifiedPlan::Command(Box::new(command.command)),
        };
        OwnedExecutablePlan::from_parts(value, memory)
    }
}

/// Both entry points retain the payload before its field reservations. The native prefix keeps
/// its original destruction workspace during every suspended fold or refused root admission.
struct Publication<P> {
    plan: P,
    fields: Option<Folding>,
    root: RootBox,
}

enum RootBox {
    Query,
    Command(Option<MemoryReservation>),
}

impl<P> Publication<P> {
    fn new(
        mut plan: P,
        borrow: impl FnOnce(&mut P) -> &mut MutableExecutablePlan,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (records, root) = match borrow(&mut plan) {
            MutableExecutablePlan::Query(query) => {
                query.assert_owner(control);
                (&mut query.memory, RootBox::Query)
            }
            MutableExecutablePlan::Command(command) => {
                let root = control.reserve(size_of::<CommandPlan>())?;
                command.assert_owner(control);
                (&mut command.memory, RootBox::Command(root))
            }
        };
        control.check()?;
        let fields = std::mem::take(records).into_parts().map(Folding::new);
        Ok(Self { plan, fields, root })
    }

    fn finish(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<(P, ExecutableMemory), SQLError> {
        control.check()?;
        let root = match self.root {
            RootBox::Query => control.reserve(size_of::<QueryPlan>())?,
            RootBox::Command(root) => root,
        };
        let storage = self.fields.take().and_then(Folding::finish);
        Ok((self.plan, ExecutableMemory::with_root(storage, root)))
    }
}

impl Prefix<MutableExecutablePlan> {
    pub(in crate::plan) async fn finish_from<W: Work + Send>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedExecutablePlan, SQLError> {
        let mut publication = Publication::new(self, |plan| plan, control)?;
        if let Some(fields) = &mut publication.fields {
            fields.advance_from(Some(control), work).await?;
        }
        let (plan, memory) = publication.finish(control)?;
        Ok(plan.take().boxed(memory))
    }
}
