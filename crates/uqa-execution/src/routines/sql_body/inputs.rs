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
    variants: super::super::plans::RoutinePlanVariants,
}

impl SQLRoutineStatement {
    fn new(definition: PreparedDefinition) -> Self {
        let variants = super::super::plans::RoutinePlanVariants::new(&definition);
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
        self.variants.select(select)
    }
}

impl SQLRoutineInputs {
    pub(crate) fn invalidate_execution_plans(&self) {
        for routine in self.routines.lock().values() {
            for statement in &routine.statements {
                statement.statement.variants.invalidate();
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
