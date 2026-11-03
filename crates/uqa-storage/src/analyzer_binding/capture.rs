//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field bindings capture each exact resolved side independently of its current analyzer name.

use super::{AnalyzerBindingOwner, BoundAnalyzerRevision, FieldAnalyzerBinding};
use crate::{
    analyzer_image::AnalyzerImageSnapshot, read_control::StorageReadControl, AnalyzerPhase,
    StorageBackendResult,
};
use std::collections::BTreeMap;
use uqa_core::memory::{Budgeted, Produced, ProductionControl, ProductionVec};

pub struct CapturedBoundAnalyzerRevision {
    pub name: Option<String>,
    pub image: AnalyzerImageSnapshot,
}

pub struct CapturedFieldAnalyzerBinding {
    pub index: CapturedBoundAnalyzerRevision,
    pub search: CapturedBoundAnalyzerRevision,
    pub owner: AnalyzerBindingOwner,
    pub last_phase: AnalyzerPhase,
}

pub type CapturedFieldAnalyzerBindings = Vec<((String, String), CapturedFieldAnalyzerBinding)>;

fn side(
    value: &BoundAnalyzerRevision,
    control: &StorageReadControl,
    production: &ProductionControl<'_>,
) -> StorageBackendResult<Produced<CapturedBoundAnalyzerRevision>> {
    let BoundAnalyzerRevision { name, compiled } = value;
    let (name, memory) = match name {
        Some(name) => {
            let (name, memory) = production.copy_text(name)?.into_parts();
            (Some(name), memory)
        }
        None => (None, production.empty_reservation()),
    };
    let image = AnalyzerImageSnapshot::capture(compiled, control)?;
    Ok(production.finish(CapturedBoundAnalyzerRevision { name, image }, memory)?)
}

pub fn fields(
    values: &BTreeMap<(String, String), FieldAnalyzerBinding>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<CapturedFieldAnalyzerBindings>> {
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let mut entries = ProductionVec::new(production);
    entries.reserve(values.len())?;
    for ((table, field), binding) in values {
        let FieldAnalyzerBinding {
            index,
            search,
            owner,
            last_phase,
        } = binding;
        let (table, table_memory) = production.copy_text(table)?.into_parts();
        let (field, field_memory) = production.copy_text(field)?.into_parts();
        let (index, index_memory) = side(index, control, &production)?.into_parts();
        let (search, search_memory) = side(search, control, &production)?.into_parts();
        let memory = production.combine(
            production.combine(table_memory, field_memory),
            production.combine(index_memory, search_memory),
        );
        entries.push_produced(production.finish(
            (
                (table, field),
                CapturedFieldAnalyzerBinding {
                    index,
                    search,
                    owner: *owner,
                    last_phase: *last_phase,
                },
            ),
            memory,
        )?)?;
    }
    Ok(entries
        .finish()?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("field analyzer capture owns an allowance")))
}

#[cfg(test)]
mod tests;
