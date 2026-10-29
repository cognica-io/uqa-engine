//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, QueryBlockPlan, SQLError, ScalarWindowSpec};
use crate::plan::scalar::OwnedWindowSpec;
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

pub fn resolved_window_definitions_with_control(
    block: &QueryBlockPlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ScalarWindowSpec>>, SQLError> {
    control.check()?;
    let mut resolved: ProductionVec<'_, ScalarWindowSpec> = ProductionVec::new(*control);
    resolved.reserve(block.windows.len())?;
    let mut names = ProductionMap::new(*control);
    for (slot, window) in block.windows.iter().enumerate() {
        control.check()?;
        if window.spec.definition.is_some() {
            return Err(invalid("a canonical specification refers to a call slot"));
        }
        if let Some(name) = &window.name {
            if name.is_empty() || names.get(&name.as_str()).is_some() {
                return Err(invalid("a canonical window name is empty or repeated"));
            }
            names.insert_copy(name.as_str(), ())?;
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
        let spec = OwnedWindowSpec::copy_parts(
            Some(slot),
            partition,
            order,
            window.spec.frame.as_ref(),
            control,
        )?
        .finish(control)?;
        resolved.push_produced(spec)?;
    }
    Ok(resolved.finish()?)
}
