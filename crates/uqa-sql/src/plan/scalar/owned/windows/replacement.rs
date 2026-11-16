//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Specification edits retain their payload before every detached constructor owner.

use super::{
    frame_children, CatalogRetentionError, OwnedScalarExpr, OwnedWindowSpec, Pending,
    ProductionControl, Result, ScalarExpr, ScalarMemory, ScalarOrder, ScalarWindowSpec, Work,
};
use crate::{ir::ownership::Retirement, plan::scalar::owned::folding::Folding};

struct Replacement {
    // Both old payload owners precede the parent that still retains their constructor leases.
    old: Option<Pending<ScalarWindowSpec>>,
    payload: Option<Retirement>,
    expression: OwnedScalarExpr,
    specification: OwnedWindowSpec,
    filter: Option<ScalarMemory>,
    retiring: Option<Folding>,
}

async fn retire_old<W: Work>(
    old: &mut Option<Pending<ScalarWindowSpec>>,
    payload: &mut Option<Retirement>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> std::result::Result<(), W::Error> {
    let specification = old.as_mut().expect("replaced window");
    for expression in specification.expressions_mut() {
        checkpoint(control, work).await?;
        *payload = Some(Retirement::new(std::mem::replace(
            expression,
            ScalarExpr::Star,
        )));
        loop {
            checkpoint(control, work).await?;
            if payload.as_mut().expect("retiring scalar").advance() {
                break;
            }
        }
        *payload = None;
    }
    while !specification.partition_by.is_empty() {
        checkpoint(control, work).await?;
        specification.partition_by.pop();
    }
    while !specification.order_by.is_empty() {
        checkpoint(control, work).await?;
        specification.order_by.pop();
    }
    drop(old.take());
    Ok(())
}

impl OwnedScalarExpr {
    pub fn replace_window_spec(
        self,
        specification: &ScalarWindowSpec,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        crate::catalog::work::inline(self.replace_window_spec_from(specification, control, &mut ()))
    }

    pub(in crate::plan) async fn replace_window_spec_from<W: Work>(
        self,
        specification: &ScalarWindowSpec,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        self.assert_owner(control);
        control.check()?;
        let specification = crate::construction::retain_with(*control, || {
            OwnedWindowSpec::copy_parts_from(
                specification.definition,
                &specification.partition_by,
                &specification.order_by,
                specification.frame.as_ref(),
                control,
                work,
            )
        })?
        .await?;
        let mut state = Replacement {
            old: None,
            payload: None,
            expression: self,
            specification,
            filter: None,
            retiring: None,
        };
        let ScalarExpr::WindowCall {
            args, spec, filter, ..
        } = &*state.expression
        else {
            panic!("window specification replacement requires a window call")
        };
        let arguments = args.len();
        let filter = filter.is_some();
        let old_count = spec.partition_by.len() + spec.order_by.len() + frame_children(spec);
        let old_bytes = spec.partition_by.capacity() * size_of::<ScalarExpr>()
            + spec.order_by.capacity() * size_of::<ScalarOrder>()
            + frame_children(spec) * size_of::<ScalarExpr>();
        let new_count = state.specification.value.partition_by.len()
            + state.specification.value.order_by.len()
            + frame_children(&state.specification.value);
        if let Some(memory) = &mut state.expression.memory {
            memory.reserve_children(new_count.saturating_sub(old_count))?;
        }
        let ScalarExpr::WindowCall { spec, .. } = &mut state.expression.expression else {
            unreachable!()
        };
        let old = std::mem::replace(spec, state.specification.value.take());
        state.old = Some(Pending::new(old));
        retire_old(&mut state.old, &mut state.payload, control, work).await?;
        if let Some(parent) = &mut state.expression.memory {
            if filter {
                state.filter = parent.children.pop().expect("window filter owner");
            }
            // Preserve the original release order while each detached subtree folds through the shared iterative owner.
            for index in arguments..arguments + old_count {
                checkpoint(control, work).await?;
                state.retiring = parent.children[index].take().map(Folding::new);
                if let Some(memory) = &mut state.retiring {
                    while !memory.is_complete() {
                        checkpoint(control, work).await?;
                        memory.advance();
                    }
                }
                drop(state.retiring.take().map(Folding::finish));
            }
            while parent.children.len() > arguments {
                checkpoint(control, work).await?;
                parent.children.pop();
            }
            drop(parent.direct.split(old_bytes));
            let replacement = state
                .specification
                .memory
                .as_mut()
                .expect("controlled window specification");
            for index in 0..new_count {
                checkpoint(control, work).await?;
                parent
                    .push_child(replacement.children[index].take())
                    .expect("window child slots admitted before replacement");
            }
            while !replacement.children.is_empty() {
                checkpoint(control, work).await?;
                replacement.children.pop();
            }
            if filter {
                parent
                    .push_child(state.filter.take())
                    .expect("retained window filter slot");
            }
            let (direct, children) = state
                .specification
                .memory
                .take()
                .expect("controlled window specification")
                .into_parts();
            drop(children);
            parent.direct.absorb(direct);
        }
        control.check()?;
        Ok(state.expression)
    }
}

pub(super) async fn checkpoint<W: Work>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> std::result::Result<(), W::Error> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    Ok(())
}
