//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! First-use SQL statement analysis, separate from source parsing and executable planning.

use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{
    prepared::definition::{analysis_is_current, PreparedDefinition, PreparedDefinitionContext},
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
    definition: Arc<PreparedDefinition>,
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

impl SQLRoutineInputs {
    pub(crate) fn invalidate(
        &self,
        affects: impl Fn(&uqa_sql::prepared::dependencies::PreparedAnalysisDependencies) -> bool,
    ) {
        let mut routines = self.routines.lock();
        for routine in routines.values_mut() {
            routine
                .statements
                .retain(|statement| !affects(&statement.definition.dependencies));
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
    ) -> Result<Arc<PreparedDefinition>, SQLError> {
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
            .map(|statement| Arc::clone(&statement.definition));
        if let Some(definition) = retained {
            if analysis_is_current(
                context,
                definition.effective_search_path.as_ref(),
                &definition.dependencies,
                definition.dependency_snapshot.as_ref(),
            )? {
                return Ok(definition);
            }
        }
        let definition = Arc::new(prepare()?);
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
            definition: Arc::clone(&definition),
        });
        Ok(definition)
    }
}

#[cfg(test)]
mod tests;
