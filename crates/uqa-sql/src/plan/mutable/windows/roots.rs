//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window consumers use the same paired scalar fields as statement rewrites.

use super::{
    checkpoint, Input, MutableQueryPlan, MutableSourcePlan, ProductionControl, SQLError,
    SourceChildSlot, SourcePlan, SourceScope, Windows,
};
use crate::plan::{
    mutable::{
        edits::Slot,
        nodes::{roots, ScalarMemory},
    },
    scalar::OwnedScalarExpr,
};

pub(super) async fn normalize<I: Input>(
    query: &mut MutableQueryPlan,
    resolved: Windows,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    if let Some(mut slot) = query.source_slot()? {
        let value = input
            .source(slot.take(), SourceScope::Roots(resolved.clone()), control)?
            .await?;
        slot.restore(value);
    }
    roots::query(
        &mut query.query,
        query.memory.as_mut(),
        roots::QueryScope::WindowUses,
        control,
        &mut Fields { input, resolved },
    )
    .await
}

struct Fields<'a, I> {
    input: &'a mut I,
    resolved: Windows,
}

impl<I: Input> roots::Input for Fields<'_, I> {
    type Work = I::Work;
    fn work(&mut self) -> &mut Self::Work {
        self.input.work()
    }
    async fn scalar(
        &mut self,
        expression: &mut crate::ScalarExpr,
        memory: Option<&mut Option<ScalarMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let mut slot = Slot::<OwnedScalarExpr>::new(expression, memory);
        let value = self
            .input
            .scalar(slot.take(), self.resolved.clone(), control)?
            .await?;
        slot.restore(value);
        Ok(())
    }
}

pub(super) async fn source<I: Input>(
    source: &mut MutableSourcePlan,
    resolved: Windows,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    if matches!(&**source, SourcePlan::Join { .. }) {
        for child in [SourceChildSlot::Left, SourceChildSlot::Right] {
            let mut slot = source.child_slot(child);
            let value = input
                .source(slot.take(), SourceScope::Roots(resolved.clone()), control)?
                .await?;
            slot.restore(value);
        }
    }
    roots::source(
        &mut source.source,
        source.memory.as_mut(),
        control,
        &mut Fields { input, resolved },
    )
    .await
}
