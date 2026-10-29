//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{override_value, Bindings, Collations};
use crate::{ast::ColumnType, schema::production::lease::SchemaMemory, SQLError};
use uqa_core::memory::ProductionControl;

impl Collations {
    pub(in crate::schema) fn column_with_control(
        &mut self,
        position: usize,
        oid: Option<u32>,
        ty: Option<&ColumnType>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let Some(value) = override_value(oid, ty) else {
            return Ok(());
        };
        if self.0.is_none() {
            let memory = control.reserve(size_of::<Bindings>())?;
            self.0 = Some(Box::new(Bindings {
                _memory: SchemaMemory::new(memory),
                ..Bindings::default()
            }));
        }
        let bindings = self.0.as_mut().expect("selected collation owner");
        if let Some(budget) = control.budget() {
            bindings.columns.insert_budgeted(position, value, budget)?;
        } else {
            bindings.columns.insert(position, value);
        }
        Ok(())
    }
}
