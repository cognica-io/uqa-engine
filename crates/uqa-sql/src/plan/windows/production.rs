//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, QueryBlockPlan, SQLError, ScalarWindowSpec};
use crate::{
    catalog::work::{self, Work as CatalogWork},
    plan::{
        retirement::{windows::checkpoint, Pending, WindowRetirement},
        scalar::{OwnedWindowSpec, ScalarInputWork as Work},
    },
    schema::retention::CatalogRetentionError,
};
use std::sync::Arc;
use uqa_core::memory::{
    MapKeyInput, MemoryReservation, Produced, ProductionControl, ProductionMap, ProductionVec,
};

#[cfg(test)]
mod tests;

pub fn resolved_window_definitions_with_control(
    block: &QueryBlockPlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ScalarWindowSpec>>, SQLError> {
    resolved_window_definitions_owned(block, control)?.finish(control)
}

pub(in crate::plan) fn resolved_window_definitions_owned(
    block: &QueryBlockPlan,
    control: &ProductionControl<'_>,
) -> Result<ResolvedWindows, SQLError> {
    work::inline(resolved_window_definitions_from(block, control, &mut ()))
}

pub(in crate::plan) struct ResolvedWindows {
    value: Pending<Vec<ScalarWindowSpec>>,
    memory: Option<MemoryReservation>,
}

impl std::ops::Deref for ResolvedWindows {
    type Target = [ScalarWindowSpec];
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl ResolvedWindows {
    pub(in crate::plan) async fn retire_shared_from<W: Work>(
        shared: Arc<Produced<Self>>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), W::Error> {
        // The query retains this final reference until every scalar/source consumer has returned.
        let (mut resolved, memory) = Arc::try_unwrap(shared)
            .unwrap_or_else(|_| unreachable!("completed window consumers released their inputs"))
            .into_parts();
        resolved.memory = control.combine(resolved.memory.take(), memory);
        struct Retirement {
            active: Option<WindowRetirement>,
            resolved: ResolvedWindows,
        }
        let mut state = Retirement {
            active: None,
            resolved,
        };
        for index in 0..state.resolved.value.len() {
            checkpoint(control, work).await?;
            let specification = std::mem::replace(
                &mut state.resolved.value[index],
                ScalarWindowSpec {
                    definition: None,
                    partition_by: Vec::new(),
                    order_by: Vec::new(),
                    frame: None,
                },
            );
            state.active = Some(WindowRetirement::new(specification));
            state
                .active
                .as_mut()
                .expect("retiring resolved window")
                .retire_from(control, work)
                .await?;
            drop(state.active.take());
        }
        while !state.resolved.value.is_empty() {
            checkpoint(control, work).await?;
            state.resolved.value.pop();
        }
        control.check()?;
        Ok(())
    }

    fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<ScalarWindowSpec>>, SQLError> {
        Ok(self.value.finish(self.memory, control)?)
    }
}

pub(in crate::plan) async fn resolved_window_definitions_from<W: Work + CatalogWork>(
    block: &QueryBlockPlan,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<ResolvedWindows, SQLError>
where
    W::Error: From<CatalogRetentionError> + Into<SQLError>,
{
    control.check()?;
    let mut buffer: ProductionVec<'_, ScalarWindowSpec> = ProductionVec::new(*control);
    buffer.reserve(block.windows.len())?;
    let (value, memory) = buffer.finish()?.into_parts();
    let mut resolved = ResolvedWindows {
        value: Pending::new(value),
        memory,
    };
    let mut names = ProductionMap::new(*control);
    for (slot, window) in block.windows.iter().enumerate() {
        control.check()?;
        CatalogWork::checkpoint(work).await?;
        control.check()?;
        if window.spec.definition.is_some() {
            return Err(invalid("a canonical specification refers to a call slot"));
        }
        if let Some(name) = &window.name {
            if name.is_empty()
                || names
                    .get_from(
                        &name.as_str(),
                        &mut Comparison {
                            work,
                            control: *control,
                        },
                    )
                    .await?
                    .is_some()
            {
                return Err(invalid("a canonical window name is empty or repeated"));
            }
            names
                .insert_copy_from(
                    name.as_str(),
                    (),
                    &mut Comparison {
                        work,
                        control: *control,
                    },
                )
                .await?;
        }
        let base = window
            .inherited
            .map(|parent| {
                resolved
                    .get(parent)
                    .filter(|_| parent < slot && block.windows[parent].name.is_some())
                    .ok_or_else(|| {
                        invalid("an inherited window is not an earlier named definition")
                    })
            })
            .transpose()?;
        if let Some(base) = base {
            if !window.spec.partition_by.is_empty()
                || (!window.spec.order_by.is_empty() && !base.order_by.is_empty())
                || base.frame.is_some()
            {
                return Err(invalid(
                    "a canonical window has incompatible inherited clauses",
                ));
            }
        }
        let partition = base.map_or(window.spec.partition_by.as_slice(), |base| {
            base.partition_by.as_slice()
        });
        let order = if window.spec.order_by.is_empty() {
            base.map_or(window.spec.order_by.as_slice(), |base| {
                base.order_by.as_slice()
            })
        } else {
            window.spec.order_by.as_slice()
        };
        let spec = OwnedWindowSpec::copy_parts_from(
            Some(slot),
            partition,
            order,
            window.spec.frame.as_ref(),
            control,
            work,
        )
        .await
        .map_err(Into::into)?
        .finish_from(control, work)
        .await
        .map_err(Into::into)?;
        let (spec, memory) = spec.into_parts();
        // All slots were admitted before resolving the first definition; attaching a completed specification cannot allocate or suspend.
        resolved.value.push(spec);
        resolved.memory = control.combine(resolved.memory.take(), memory);
    }
    control.check()?;
    Ok(resolved)
}

struct Comparison<'a, 'b, W> {
    work: &'a mut W,
    control: ProductionControl<'b>,
}
impl<'q, 's, W: CatalogWork + Send> MapKeyInput<&'q str, &'s str> for Comparison<'_, '_, W> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(CatalogWork::checkpoint(self.work).await?)
    }
    async fn compare(
        &mut self,
        left: &&'q str,
        right: &&'s str,
    ) -> Result<std::cmp::Ordering, Self::Error> {
        work::compare_text(self.work, left, right, &self.control).await
    }
}
