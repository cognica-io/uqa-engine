//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Produced, ProductionControl, Result};
use crate::ColumnIdentity;

impl ColumnIdentity {
    pub(crate) async fn copy_from_work<W: crate::catalog::work::Work + Send>(
        qualifier: Option<&str>,
        column: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Produced<Self>, crate::SQLError> {
        let mut source = crate::catalog::work::ReferenceWork { control, work };
        let qualifier = match qualifier {
            Some(qualifier) => {
                source.checkpoint().await?;
                let (value, memory) = source.work.text(qualifier, control).await?.into_parts();
                (Some(value.into_boxed_str()), memory)
            }
            None => (None, control.empty_reservation()),
        };
        source.checkpoint().await?;
        let (column, memory) = source.work.text(column, control).await?.into_parts();
        Ok(control.finish(
            Self {
                qualifier: qualifier.0,
                column: column.into_boxed_str(),
            },
            control.combine(qualifier.1, memory),
        )?)
    }
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

    pub fn clone_with_control(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>> {
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

impl<W: crate::catalog::work::Work + Send> uqa_core::memory::MapKeyInput<ColumnIdentity>
    for crate::catalog::work::ReferenceWork<'_, '_, W>
{
    type Error = crate::SQLError;

    async fn checkpoint(&mut self) -> std::result::Result<(), Self::Error> {
        Self::checkpoint(self).await
    }

    async fn compare(
        &mut self,
        left: &ColumnIdentity,
        right: &ColumnIdentity,
    ) -> std::result::Result<std::cmp::Ordering, Self::Error> {
        use crate::catalog::work::compare_text;
        use std::cmp::Ordering;
        self.checkpoint().await?;
        let qualifier = match (left.qualifier(), right.qualifier()) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(left), Some(right)) => compare_text(self.work, left, right, self.control).await?,
        };
        if qualifier.is_eq() {
            compare_text(self.work, left.column(), right.column(), self.control).await
        } else {
            Ok(qualifier)
        }
    }
}
