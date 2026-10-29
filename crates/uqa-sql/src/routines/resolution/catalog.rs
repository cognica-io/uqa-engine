//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine resolution borrows native catalog generations and produces independent candidate snapshots.

use super::{search_path_precedes_pg_catalog, RoutineCallKind};
use crate::{
    ast::FunctionBinding,
    catalog::domain::StoredDomain,
    routines::{declaration::RoutineTypeCatalog, SQLUserFunction},
    SQLError,
};
use std::{collections::BTreeMap, sync::Arc};

/// The original immutable domain allocation captured for one signature-matching pass.
pub type RoutineTypeSnapshot = Arc<BTreeMap<String, StoredDomain>>;
pub trait RoutineOverloadCatalog: RoutineTypeCatalog + Send + Sync {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot;
    fn routine_search_path(&self) -> Vec<String>;
    /// Observe namespace precedence without copying session state in native catalogs.
    fn routine_schema_precedes_pg_catalog(
        &self,
        schema: uqa_core::RelationName<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        search_path_precedes_pg_catalog(
            self.routine_search_path().iter().map(String::as_str),
            schema,
            control,
        )
    }
    fn has_registered_scalar_function(&self, name: &str) -> bool;
    fn has_registered_scalar_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        Ok(self.has_registered_scalar_function(name))
    }
    fn lookup_sql_routine_candidates(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError>;
    fn lookup_sql_routine_candidates_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<Arc<SQLUserFunction>>>>, SQLError> {
        control.check()?;
        let Some(candidates) = self.lookup_sql_routine_candidates(name)? else {
            return Ok(None);
        };
        if control.budget().is_none() {
            return Ok(Some(control.finish(candidates, None)?));
        }
        let mut output = uqa_core::memory::ProductionVec::new(*control);
        output.reserve(candidates.len())?;
        for candidate in candidates {
            output.push_produced(control.finish(candidate, control.empty_reservation())?)?;
        }
        Ok(Some(output.finish()?))
    }
    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>>;
    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>>;
    /// Select a bound identity; native registries borrow their retained candidates.
    fn select_bound_routine_with_control(
        &self,
        binding: &FunctionBinding,
        kind: RoutineCallKind,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        control.check()?;
        let candidates = self.lookup_bound_sql_routine_candidates_by_binding(binding);
        crate::routines::lifecycle::lookup::select_bound_routine_with_control(
            candidates.iter().flatten(),
            binding,
            kind,
            control,
        )
    }
}
