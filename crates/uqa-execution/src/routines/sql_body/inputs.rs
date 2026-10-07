//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! First-use SQL analysis and executable variants, retained by their routine owner.

use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{
    prepared::{
        definition::{analysis_is_current, PreparedDefinition, PreparedDefinitionContext},
        entry::PreparedStatementPlan,
    },
    routines::SQLUserFunction,
    ColumnType, SQLError,
};

#[derive(Clone, Copy)]
pub struct SQLBodyIdentity {
    object: [u8; 16],
    version: u64,
}

impl SQLBodyIdentity {
    pub fn new(function: &SQLUserFunction) -> Result<Self, SQLError> {
        Ok(Self {
            object: function
                .def
                .object_id
                .ok_or_else(|| SQLError::Internal("SQL body has no routine identity".into()))?,
            version: function.definition_version()?,
        })
    }
}

struct StatementInputs {
    position: usize,
    parameter_types: Vec<ColumnType>,
    statement: Arc<SQLRoutineStatement>,
}

struct RoutineInputs {
    version: u64,
    statements: Vec<StatementInputs>,
}

/// Session-local analysis by routine revision, statement position and concrete input types.
#[derive(Default)]
pub struct SQLRoutineInputs {
    routines: Mutex<BTreeMap<[u8; 16], RoutineInputs>>,
}

pub struct SQLRoutineInputContext<'a> {
    pub cache: &'a SQLRoutineInputs,
    pub analysis: PreparedDefinitionContext<'a>,
}

/// One successful input analysis owns its custom/generic history. Replacement
/// analysis starts a new owner, so an older activation cannot republish its plan.
pub struct SQLRoutineStatement {
    pub definition: PreparedDefinition,
    variants: Mutex<PreparedStatementPlan>,
}

impl SQLRoutineStatement {
    fn new(definition: PreparedDefinition) -> Self {
        let logical_plan = Arc::new(definition.logical_plan.clone());
        let variants = Mutex::new(PreparedStatementPlan {
            source_plan: Arc::clone(&logical_plan),
            logical_plan,
            needs_analysis: false,
            effective_search_path: definition.effective_search_path.clone(),
            dependencies: definition.dependencies.clone(),
            dependency_snapshot: definition.dependency_snapshot.clone(),
            plan: None,
            parameter_types: definition.parameter_types.clone(),
            result_schema: definition.result_schema.clone(),
            source_sql: None,
            prepared_at_micros: 0,
            from_sql: false,
            generic_plans: 0,
            custom_plans: 0,
            generic_cost: None,
            total_custom_cost: 0.0,
        });
        Self {
            definition,
            variants,
        }
    }

    pub(super) fn select(
        &self,
        select: impl FnOnce(
            &PreparedStatementPlan,
        )
            -> Result<uqa_sql::prepared::planning::PreparedPlanSelection, SQLError>,
    ) -> Result<uqa_sql::plan::UnifiedPlan, SQLError> {
        let entry = self.variants.lock().clone();
        let selected = select(&entry)?;
        let mut current = self.variants.lock();
        if Arc::ptr_eq(&current.logical_plan, &entry.logical_plan) {
            current.record_execution(selected.update);
        }
        Ok(selected.plan)
    }
}

impl SQLRoutineInputs {
    pub(crate) fn invalidate_execution_plans(&self) {
        for routine in self.routines.lock().values() {
            for statement in &routine.statements {
                let mut entry = statement.statement.variants.lock();
                if !entry.has_tracked_executable_dependencies() {
                    entry.plan = None;
                    // An optimizer callback may publish a catalog change. Give
                    // the surviving analysis a fresh publication identity so
                    // that callback cannot restore a discarded executable.
                    entry.logical_plan = Arc::new((*entry.logical_plan).clone());
                }
            }
        }
    }

    pub(crate) fn invalidate(
        &self,
        affects: impl Fn(&uqa_sql::prepared::dependencies::PreparedAnalysisDependencies) -> bool,
    ) {
        let mut routines = self.routines.lock();
        for routine in routines.values_mut() {
            routine
                .statements
                .retain(|statement| !affects(&statement.statement.definition.dependencies));
        }
        routines.retain(|_, routine| !routine.statements.is_empty());
    }

    /// Publish only successful analysis and result validation, before execution.
    /// Neither catalog callbacks nor recursive preparation run under the cache lock.
    pub fn statement(
        &self,
        identity: SQLBodyIdentity,
        position: usize,
        parameter_types: &[ColumnType],
        context: &PreparedDefinitionContext<'_>,
        prepare: impl FnOnce() -> Result<PreparedDefinition, SQLError>,
    ) -> Result<Arc<SQLRoutineStatement>, SQLError> {
        let retained = self
            .routines
            .lock()
            .get(&identity.object)
            .filter(|routine| routine.version == identity.version)
            .and_then(|routine| {
                routine.statements.iter().find(|statement| {
                    statement.position == position && statement.parameter_types == parameter_types
                })
            })
            .map(|statement| Arc::clone(&statement.statement));
        if let Some(statement) = retained {
            let definition = &statement.definition;
            if analysis_is_current(
                context,
                definition.effective_search_path.as_ref(),
                &definition.dependencies,
                definition.dependency_snapshot.as_ref(),
            )? {
                return Ok(statement);
            }
        }
        let statement = Arc::new(SQLRoutineStatement::new(prepare()?));
        let mut routines = self.routines.lock();
        let routine = routines
            .entry(identity.object)
            .or_insert_with(|| RoutineInputs {
                version: identity.version,
                statements: Vec::new(),
            });
        if routine.version != identity.version {
            routine.version = identity.version;
            routine.statements.clear();
        }
        routine.statements.retain(|statement| {
            statement.position != position || statement.parameter_types != parameter_types
        });
        routine.statements.push(StatementInputs {
            position,
            parameter_types: parameter_types.to_vec(),
            statement: Arc::clone(&statement),
        });
        Ok(statement)
    }
}

#[cfg(test)]
mod tests;
