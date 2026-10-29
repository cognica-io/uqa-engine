//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Produced, ProductionControl, Result};
use crate::ColumnIdentity;

impl ColumnIdentity {
    /// Construct one structured SQL identity after admitting its exact boxed text destinations.
    pub fn from_parts_with_control(
        qualifier: Option<&str>,
        column: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        let column_memory = control.reserve(column.len())?;
        let qualifier_memory = control.reserve(qualifier.map_or(0, str::len))?;
        Ok(control.finish(
            Self {
                qualifier: qualifier.map(Box::from),
                column: Box::from(column),
            },
            control.combine(column_memory, qualifier_memory),
        )?)
    }

    pub(crate) fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        let column = control.reserve(self.column.len())?;
        let qualifier = control.reserve(self.qualifier.as_ref().map_or(0, |name| name.len()))?;
        Ok(control.finish(
            Self {
                qualifier: self.qualifier.as_deref().map(Box::from),
                column: Box::from(self.column.as_ref()),
            },
            control.combine(column, qualifier),
        )?)
    }
}
