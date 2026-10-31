//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed catalog declarations separate inline metadata from allocation-bearing type construction.

use super::VirtualRelation;
use crate::{ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) enum CatalogColumnType {
    Inline(ColumnType),
    Array(ColumnType),
    Domain(u32),
}
impl CatalogColumnType {
    fn produce(self, control: &ProductionControl<'_>) -> Result<Produced<ColumnType>, SQLError> {
        Ok(match self {
            Self::Inline(ty) => control.finish(ty, control.empty_reservation())?,
            Self::Array(element) => ColumnType::array_with_control(
                control.finish(element, control.empty_reservation())?,
                control,
            )?,
            Self::Domain(oid) => super::system_catalog_domain_with_control(oid, control)?
                .expect("fixed catalog domain"),
        })
    }
}
impl VirtualRelation {
    pub fn schema_with_control(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<(String, ColumnType)>>, SQLError> {
        control.check()?;
        let mut columns = ProductionVec::new(*control);
        let mut status: Result<(), SQLError> = Ok(());
        self.visit_columns(&mut |name, ty| {
            if status.is_ok() {
                status = (|| {
                    let (name, name_memory) = control.copy_text(name)?.into_parts();
                    let (ty, type_memory) = ty.produce(control)?.into_parts();
                    columns.push_produced(
                        control.finish((name, ty), control.combine(name_memory, type_memory))?,
                    )?;
                    Ok(())
                })();
            }
        });
        status?;
        Ok(columns.finish()?)
    }
}

#[cfg(test)]
mod tests;
