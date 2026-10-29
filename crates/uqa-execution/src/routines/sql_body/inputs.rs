//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! First-use SQL analysis and executable variants, retained by their routine owner.

use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::memory::{MemoryReservation, OwnedMap, Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    prepared::{
        definition::{
            analysis_is_current_with_control, PreparedDefinition, PreparedDefinitionContext,
        },
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
    parameter_types: Produced<Vec<ColumnType>>,
    statement: Arc<SQLRoutineStatement>,
}

struct RoutineInputs {
    version: u64,
    statements: OwnedMap<usize, StatementInputs>,
}

/// Session-local analysis by routine revision, statement position and concrete input types.
#[derive(Default)]
pub struct SQLRoutineInputs {
    routines: Mutex<OwnedMap<[u8; 16], RoutineInputs>>,
}

pub struct SQLRoutineInputContext<'a> {
    pub cache: &'a SQLRoutineInputs,
    pub analysis: PreparedDefinitionContext<'a>,
}

/// One successful input analysis owns its custom/generic history. Replacement
/// analysis starts a new owner, so an older activation cannot republish its plan.
pub struct SQLRoutineStatement {
    pub definition: PreparedDefinition<uqa_sql::plan::SharedExecutablePlan>,
    variants: super::super::plans::RoutinePlanVariants,
    _memory: Option<MemoryReservation>,
}

impl SQLRoutineStatement {
    fn new(
        definition: PreparedDefinition<uqa_sql::plan::SharedExecutablePlan>,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<Self>, SQLError> {
        let memory = control.reserve(size_of::<Self>())?;
        let variants = super::super::plans::RoutinePlanVariants::new(&definition);
        Ok(Arc::new(Self {
            definition,
            variants,
            _memory: memory,
        }))
    }

    pub(super) fn select(
        &self,
        select: impl FnOnce(
            &PreparedStatementPlan,
        )
            -> Result<uqa_sql::prepared::planning::PreparedPlanSelection, SQLError>,
    ) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError> {
        self.variants.select(select)
    }
}

impl SQLRoutineInputs {
    pub(crate) fn invalidate_execution_plans(&self) {
        for routine in self.routines.lock().values() {
            for statement in routine.statements.values() {
                statement.statement.variants.invalidate();
            }
        }
    }

    pub(crate) fn invalidate(
        &self,
        affects: impl Fn(&uqa_sql::prepared::dependencies::PreparedAnalysisDependencies) -> bool,
    ) {
        let mut routines = self.routines.lock();
        routines.retain(|_, routine| {
            routine
                .statements
                .retain(|_, statement| !affects(&statement.statement.definition.dependencies));
            !routine.statements.is_empty()
        });
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
        self.statement_with_control(
            identity,
            position,
            parameter_types,
            context,
            &ProductionControl::uncontrolled(),
            || prepare().map(PreparedDefinition::into_shared),
        )
    }

    pub fn statement_with_control(
        &self,
        identity: SQLBodyIdentity,
        position: usize,
        parameter_types: &[ColumnType],
        context: &PreparedDefinitionContext<'_>,
        control: &ProductionControl<'_>,
        prepare: impl FnOnce() -> Result<
            PreparedDefinition<uqa_sql::plan::SharedExecutablePlan>,
            SQLError,
        >,
    ) -> Result<Arc<SQLRoutineStatement>, SQLError> {
        control.check()?;
        let retained = self
            .routines
            .lock()
            .get(&identity.object)
            .filter(|routine| routine.version == identity.version)
            .and_then(|routine| {
                routine.statements.values().find(|statement| {
                    statement.position == position
                        && statement.parameter_types.as_slice() == parameter_types
                })
            })
            .map(|statement| Arc::clone(&statement.statement));
        if let Some(statement) = retained {
            let definition = &statement.definition;
            if analysis_is_current_with_control(
                context,
                definition.effective_search_path.as_ref(),
                &definition.dependencies,
                definition.dependency_snapshot.as_ref(),
                control,
            )? {
                return Ok(statement);
            }
        }
        let statement = SQLRoutineStatement::new(prepare()?, control)?;
        let mut types = ProductionVec::new(*control);
        types.reserve(parameter_types.len())?;
        for ty in parameter_types {
            types.push_produced(ty.clone_with_control(control)?)?;
        }
        let input = StatementInputs {
            position,
            parameter_types: types.finish()?,
            statement: Arc::clone(&statement),
        };
        let mut routines = self.routines.lock();
        if let Some(routine) = routines
            .get_mut(&identity.object)
            .filter(|routine| routine.version == identity.version)
        {
            let slot = routine
                .statements
                .iter()
                .find(|(_, input)| {
                    input.position == position
                        && input.parameter_types.as_slice() == parameter_types
                })
                .map(|(slot, _)| *slot);
            let slot = match slot {
                Some(slot) => slot,
                None => routine
                    .statements
                    .keys()
                    .copied()
                    .max()
                    .map_or(Ok(0), |last| {
                        last.checked_add(1).ok_or_else(|| {
                            SQLError::Internal("SQL routine statement cache is full".into())
                        })
                    })?,
            };
            insert(&mut routine.statements, slot, input, control)?;
        } else {
            let mut selected = OwnedMap::new();
            insert(&mut selected, 0, input, control)?;
            insert(
                &mut routines,
                identity.object,
                RoutineInputs {
                    version: identity.version,
                    statements: selected,
                },
                control,
            )?;
        }
        Ok(statement)
    }
}

fn insert<K: Ord, V>(
    map: &mut OwnedMap<K, V>,
    key: K,
    value: V,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match control.budget() {
        Some(budget) => {
            map.insert_budgeted(key, value, budget)?;
        }
        None => {
            map.insert(key, value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
