//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive working rows and control widths publish and restore as one lexical binding.

use super::{
    bindings::{restore_cte_names, save_and_remove_cte_names_with_control, SavedCteBindings},
    CteRowsMap, CteScope,
};
use uqa_core::memory::ProductionControl;
use uqa_sql::{binding::production::BindingMap, SQLError};

pub struct RecursiveCteScope<'a, 'n, S: Clone> {
    scope: &'a mut CteScope<S>,
    name: &'n str,
    saved: Option<SavedCteBindings>,
    width: Option<BindingMap<usize>>,
}

impl<S: Clone> CteScope<S> {
    /// Complete both destinations before changing the scope. Saved nodes restore the parent on success, error and unwind without consuming more allowance.
    pub fn enter_recursive_cte_with_control<'a, 'n>(
        &'a mut self,
        name: &'n str,
        rows: crate::SharedSpill,
        width: Option<usize>,
        control: &ProductionControl<'_>,
    ) -> Result<RecursiveCteScope<'a, 'n, S>, SQLError> {
        let mut pending_rows = CteRowsMap::new();
        pending_rows.insert_with_control(
            name,
            crate::query::cte::rows::CteRows::Complete(rows),
            control,
        )?;
        let mut pending_width = BindingMap::new();
        if let Some(width) = width {
            pending_width.insert_with_control(name, width, control)?;
        }
        let saved = save_and_remove_cte_names_with_control(self, [name], control)?;
        let previous_width = width.map(|_| self.recursive_control_widths.extract(name));
        self.rows.append(pending_rows);
        self.recursive_control_widths.append(pending_width);
        Ok(RecursiveCteScope {
            scope: self,
            name,
            saved: Some(saved),
            width: previous_width,
        })
    }
}

impl<S: Clone> std::ops::Deref for RecursiveCteScope<'_, '_, S> {
    type Target = CteScope<S>;
    fn deref(&self) -> &Self::Target {
        self.scope
    }
}
impl<S: Clone> std::ops::DerefMut for RecursiveCteScope<'_, '_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.scope
    }
}
impl<S: Clone> Drop for RecursiveCteScope<'_, '_, S> {
    fn drop(&mut self) {
        if let Some(width) = self.width.take() {
            self.scope.recursive_control_widths.remove(self.name);
            self.scope.recursive_control_widths.append(width);
        }
        restore_cte_names(
            self.scope,
            self.saved.take().expect("live recursive binding"),
        );
    }
}
