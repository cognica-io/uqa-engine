//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Produced, ProductionControl, Result};
use crate::ColumnIdentity;

impl ColumnIdentity {
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
