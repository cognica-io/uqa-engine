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
                memory: SchemaMemory::new(memory),
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

impl Collations {
    pub(in crate::schema) fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let Some(input) = self.0.as_deref() else {
            return Ok(Self::default());
        };
        let mut memory = control.reserve(size_of::<Bindings>())?;
        let mut output = Bindings::default();
        for (position, value) in &input.columns {
            insert(&mut output.columns, *position, *value, control)?;
        }
        for (identity, value) in &input.aliases {
            let (identity, lease) = identity.clone_with_control(control)?.into_parts();
            insert(&mut output.aliases, identity, *value, control)?;
            memory = control.combine(memory, lease);
        }
        for (identity, value) in &input.virtual_columns {
            let (identity, lease) = identity.clone_with_control(control)?.into_parts();
            insert(&mut output.virtual_columns, identity, *value, control)?;
            memory = control.combine(memory, lease);
        }
        for (identity, value) in &input.internal {
            insert(&mut output.internal, *identity, *value, control)?;
        }
        output.memory = SchemaMemory::new(memory);
        let output = Self(Some(Box::new(output)));
        control.check()?;
        Ok(output)
    }
}

pub(super) fn insert<K: Ord>(
    map: &mut uqa_core::memory::OwnedMap<K, u32>,
    key: K,
    value: u32,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(budget) = control.budget() {
        map.insert_budgeted(key, value, budget)?;
    } else {
        map.insert(key, value);
    }
    Ok(())
}

impl Collations {
    pub(in crate::schema) fn internal_with_control(
        &mut self,
        column: crate::ast::InternalColumnRef,
        oid: Option<u32>,
        ty: Option<&ColumnType>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let Some(value) = override_value(oid, ty) else {
            if let Some(bindings) = self.0.as_mut() {
                bindings.internal.remove(&column);
            }
            return Ok(());
        };
        if self.0.is_none() {
            let memory = control.reserve(size_of::<Bindings>())?;
            self.0 = Some(Box::new(Bindings {
                memory: SchemaMemory::new(memory),
                ..Bindings::default()
            }));
        }
        insert(
            &mut self
                .0
                .as_mut()
                .expect("selected internal collation")
                .internal,
            column,
            value,
            control,
        )
    }
}
