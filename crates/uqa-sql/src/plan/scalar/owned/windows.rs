//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window specification replacement preserves argument/filter owners and retires only the superseded specification allocations.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{
    plan::{
        retirement::Pending,
        scalar::resources::{input::Work, Control, Lowering},
    },
    schema::retention::CatalogRetentionError,
    ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};
use uqa_core::memory::{Produced, ProductionControl};

mod normalization;
mod replacement;

#[cfg(test)]
mod tests;

pub(in crate::plan) struct OwnedWindowSpec {
    value: Pending<ScalarWindowSpec>,
    memory: Option<ScalarMemory>,
}
impl OwnedWindowSpec {
    #[cfg(test)]
    pub(in crate::plan) fn copy_parts(
        definition: Option<usize>,
        partition: &[ScalarExpr],
        order: &[ScalarOrder],
        frame: Option<&ScalarWindowFrame>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        crate::catalog::work::inline(Self::copy_parts_from(
            definition,
            partition,
            order,
            frame,
            control,
            &mut (),
        ))
    }

    pub(in crate::plan) async fn copy_parts_from<W: Work>(
        definition: Option<usize>,
        partition: &[ScalarExpr],
        order: &[ScalarOrder],
        frame: Option<&ScalarWindowFrame>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        control.check()?;
        crate::construction::retain_with(*control, || {
            Self::construct_from(definition, partition, order, frame, control, work)
        })?
        .await
    }

    async fn construct_from<W: Work>(
        definition: Option<usize>,
        partition: &[ScalarExpr],
        order: &[ScalarOrder],
        frame: Option<&ScalarWindowFrame>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        let mut lowering = Lowering {
            control: control.budget().map(|_| Control::from_production(*control)),
            owned_children: control
                .budget()
                .map(|budget| ScalarMemory::new(budget.empty_reservation())),
        };
        let mut value = Pending::new(ScalarWindowSpec {
            definition,
            partition_by: Vec::new(),
            order_by: Vec::new(),
            frame: None,
        });
        lowering.checkpoint(work).await?;
        value.partition_by = lowering.vector(partition.len())?;
        for expression in partition {
            lowering.checkpoint(work).await?;
            let child = expression.copy_for_rewrite_from(control, work).await?;
            value
                .partition_by
                .push(OwnedScalarExpr::attach_child(&mut lowering, child)?);
        }
        lowering.checkpoint(work).await?;
        value.order_by = lowering.vector(order.len())?;
        for order in order {
            lowering.checkpoint(work).await?;
            let child = order.expr.copy_for_rewrite_from(control, work).await?;
            value.order_by.push(ScalarOrder {
                expr: OwnedScalarExpr::attach_child(&mut lowering, child)?,
                descending: order.descending,
                nulls: order.nulls,
            });
        }
        if let Some(frame) = frame {
            value.frame = Some(ScalarWindowFrame {
                mode: frame.mode,
                start: ScalarFrameBound::CurrentRow,
                end: ScalarFrameBound::CurrentRow,
                between: frame.between,
                exclusion: frame.exclusion,
            });
            let target = value
                .frame
                .as_mut()
                .expect("window frame under construction");
            target.start = bound_from(&mut lowering, &frame.start, control, work).await?;
            target.end = bound_from(&mut lowering, &frame.end, control, work).await?;
        }
        let memory = lowering.control.map(|control| {
            ScalarMemory::from_parts(
                control.memory,
                lowering.owned_children.expect("controlled window children"),
            )
        });
        let output = Self { value, memory };
        control.check()?;
        Ok(output)
    }
    #[cfg(test)]
    pub(in crate::plan) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ScalarWindowSpec>> {
        crate::catalog::work::inline(self.finish_from(control, &mut ()))
    }

    pub(in crate::plan) async fn finish_from<W: Work>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Produced<ScalarWindowSpec>, W::Error> {
        super::publication::Publication::from_parts(self.value, self.memory)
            .finish_from(control, work)
            .await
    }
}
async fn bound_from<W: Work>(
    lowering: &mut Lowering<'_>,
    value: &ScalarFrameBound,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> std::result::Result<ScalarFrameBound, W::Error>
where
    W::Error: From<CatalogRetentionError>,
{
    lowering.checkpoint(work).await?;
    Ok(match value {
        ScalarFrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
        ScalarFrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
        ScalarFrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            lowering.admit_box::<ScalarExpr>()?;
            let child = expression.copy_for_rewrite_from(control, work).await?;
            let child = Box::new(OwnedScalarExpr::attach_child(lowering, child)?);
            if matches!(value, ScalarFrameBound::Preceding(_)) {
                ScalarFrameBound::Preceding(child)
            } else {
                ScalarFrameBound::Following(child)
            }
        }
    })
}
fn frame_children(spec: &ScalarWindowSpec) -> usize {
    spec.frame
        .iter()
        .flat_map(|frame| [&frame.start, &frame.end])
        .filter(|bound| {
            matches!(
                bound,
                ScalarFrameBound::Preceding(_) | ScalarFrameBound::Following(_)
            )
        })
        .count()
}
