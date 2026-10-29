//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window specification replacement preserves argument/filter owners and retires only the superseded specification allocations.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{
    plan::scalar::resources::{Control, Lowering},
    ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};

pub(in crate::plan) struct OwnedWindowSpec {
    value: ScalarWindowSpec,
    memory: Option<ScalarMemory>,
}
impl OwnedWindowSpec {
    pub(in crate::plan) fn copy_parts(
        definition: Option<usize>,
        partition: &[ScalarExpr],
        order: &[ScalarOrder],
        frame: Option<&ScalarWindowFrame>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let mut lowering = Lowering {
            control: control.budget().map(|_| Control::from_production(*control)),
            owned_children: control.budget().map(BudgetedVec::new),
        };
        let value = ScalarWindowSpec {
            definition,
            partition_by: lowering.map(partition.iter(), |lowering, expression| {
                OwnedScalarExpr::attach_child(lowering, expression.copy_for_rewrite(control)?)
            })?,
            order_by: lowering.map(order.iter(), |lowering, order| {
                Ok(ScalarOrder {
                    expr: OwnedScalarExpr::attach_child(
                        lowering,
                        order.expr.copy_for_rewrite(control)?,
                    )?,
                    descending: order.descending,
                    nulls: order.nulls,
                })
            })?,
            frame: frame
                .map(|frame| {
                    Ok::<_, crate::schema::retention::CatalogRetentionError>(ScalarWindowFrame {
                        mode: frame.mode,
                        start: bound(&mut lowering, &frame.start, control)?,
                        end: bound(&mut lowering, &frame.end, control)?,
                        between: frame.between,
                        exclusion: frame.exclusion,
                    })
                })
                .transpose()?,
        };
        let memory = lowering.control.map(|control| ScalarMemory {
            direct: control.memory,
            children: lowering.owned_children.expect("controlled window children"),
        });
        let output = Self { value, memory };
        control.check()?;
        Ok(output)
    }
    pub(in crate::plan) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ScalarWindowSpec>> {
        Ok(control.finish(self.value, self.memory.map(ScalarMemory::into_reservation))?)
    }
}
fn bound(
    lowering: &mut Lowering<'_>,
    value: &ScalarFrameBound,
    control: &ProductionControl<'_>,
) -> Result<ScalarFrameBound> {
    Ok(match value {
        ScalarFrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
        ScalarFrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
        ScalarFrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
        ScalarFrameBound::Preceding(expression) => {
            ScalarFrameBound::Preceding(lowering.boxed(|lowering| {
                OwnedScalarExpr::attach_child(lowering, expression.copy_for_rewrite(control)?)
            })?)
        }
        ScalarFrameBound::Following(expression) => {
            ScalarFrameBound::Following(lowering.boxed(|lowering| {
                OwnedScalarExpr::attach_child(lowering, expression.copy_for_rewrite(control)?)
            })?)
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

impl OwnedScalarExpr {
    pub fn replace_window_spec(
        mut self,
        specification: &ScalarWindowSpec,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let replacement = OwnedWindowSpec::copy_parts(
            specification.definition,
            &specification.partition_by,
            &specification.order_by,
            specification.frame.as_ref(),
            control,
        )?;
        let ScalarExpr::WindowCall { args, spec, .. } = &self.expression else {
            panic!("window specification replacement requires a window call")
        };
        let arguments = args.len();
        let old_count = spec.partition_by.len() + spec.order_by.len() + frame_children(spec);
        let old_bytes = spec.partition_by.capacity() * size_of::<ScalarExpr>()
            + spec.order_by.capacity() * size_of::<ScalarOrder>()
            + frame_children(spec) * size_of::<ScalarExpr>();
        let new_count = replacement.value.partition_by.len()
            + replacement.value.order_by.len()
            + frame_children(&replacement.value);
        if let Some(memory) = &mut self.memory {
            memory
                .children
                .reserve(new_count.saturating_sub(old_count))?;
        }
        let ScalarExpr::WindowCall { spec, .. } = &mut self.expression else {
            unreachable!()
        };
        let old = std::mem::replace(spec, replacement.value);
        drop(old);
        self.remove_child_owners(arguments..arguments + old_count);
        if let Some(parent) = &mut self.memory {
            drop(parent.direct.split(old_bytes));
            let ScalarMemory { direct, children } =
                replacement.memory.expect("controlled specification");
            let (children, children_memory) = children.into_parts();
            for child in children {
                parent
                    .children
                    .push(child)
                    .expect("window child slots admitted before replacement");
            }
            drop(children_memory);
            parent.children[arguments..].rotate_right(new_count);
            parent.direct.absorb(direct);
        }
        control.check()?;
        Ok(self)
    }
}
