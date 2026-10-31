//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table sources retain admitted names and layout buffers until their final consumer drops them.

use super::{HierarchyRowSource, LocalTableRowSource, LocalTableScanConfig};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::SQLError;

pub(super) fn copy_names(
    names: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for name in names {
        output.push_produced(control.copy_text(name)?)?;
    }
    Ok(output.finish()?)
}

impl LocalTableRowSource {
    pub(crate) fn from_produced(config: Produced<LocalTableScanConfig>) -> Self {
        let (config, memory) = config.into_parts();
        let mut source = Self::new(config);
        source.memory = memory;
        source
    }

    pub(crate) fn into_boxed(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Box<Self>, SQLError> {
        self.memory = control.combine(self.memory.take(), control.reserve(size_of::<Self>())?);
        Ok(Box::new(self))
    }
}

impl HierarchyRowSource {
    pub(crate) fn boxed_with_control(
        sources: Produced<Vec<LocalTableRowSource>>,
        control: &ProductionControl<'_>,
    ) -> Result<Box<Self>, SQLError> {
        let first = sources.first().ok_or_else(|| {
            SQLError::Internal("inheritance scan was built without a physical table".into())
        })?;
        let schema = copy_names(&first.schema, control)?;
        let physical_schema = first.physical_schema.clone();
        let estimated_cardinality = sources
            .iter()
            .map(|source| source.estimated_cardinality)
            .sum();
        let header = control.reserve(size_of::<Self>())?;
        let (sources, source_memory) = sources.into_parts();
        let (schema, schema_memory) = schema.into_parts();
        Ok(Box::new(Self {
            sources: sources.into(),
            schema,
            physical_schema,
            estimated_cardinality,
            _memory: control.combine(control.combine(source_memory, schema_memory), header),
        }))
    }
}
