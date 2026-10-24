//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared registry publication, session invalidation and registration retirement.

use super::{RegisteredSQLFunction, SQLAggregateFactory, SQLScalarFunction, SQLTableFunction};
use crate::{state::StatementGate, SQLError, SessionContext};
use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

#[derive(Debug, thiserror::Error)]
pub enum SQLFunctionRegistrationError {
    #[error("SQL function registry has active execution or retained function resources")]
    Busy,
    #[error(transparent)]
    Invalid(#[from] SQLError),
}

impl From<SQLFunctionRegistrationError> for SQLError {
    fn from(error: SQLFunctionRegistrationError) -> Self {
        match error {
            SQLFunctionRegistrationError::Busy => Self::Routine {
                sqlstate: "55006".into(),
                message: error.to_string(),
            },
            SQLFunctionRegistrationError::Invalid(error) => error,
        }
    }
}

type Result<T> = std::result::Result<T, SQLFunctionRegistrationError>;
type Map<F> = RwLock<BTreeMap<String, RegisteredSQLFunction<F>>>;

enum Function {
    Scalar(Arc<dyn SQLScalarFunction>),
    Table(Arc<dyn SQLTableFunction>),
    Aggregate(Arc<dyn SQLAggregateFactory>),
}

enum WeakFunction {
    Scalar(Weak<dyn SQLScalarFunction>),
    Table(Weak<dyn SQLTableFunction>),
    Aggregate(Weak<dyn SQLAggregateFactory>),
}

impl WeakFunction {
    fn upgrade(&self) -> Option<Function> {
        match self {
            Self::Scalar(function) => function.upgrade().map(Function::Scalar),
            Self::Table(function) => function.upgrade().map(Function::Table),
            Self::Aggregate(function) => function.upgrade().map(Function::Aggregate),
        }
    }
}

struct Session {
    context: Weak<SessionContext>,
    gate: Weak<StatementGate>,
}

#[derive(Default)]
struct Members {
    sessions: Vec<Session>,
    functions: Vec<WeakFunction>,
}

pub(crate) struct Registry {
    pub(crate) scalar: Map<dyn SQLScalarFunction>,
    pub(crate) table: Map<dyn SQLTableFunction>,
    pub(crate) aggregate: Map<dyn SQLAggregateFactory>,
    members: Mutex<Members>,
    revision: AtomicU64,
}

impl Registry {
    pub(crate) fn new(session: &Arc<SessionContext>, gate: &Arc<StatementGate>) -> Self {
        let registry = Self {
            scalar: RwLock::default(),
            table: RwLock::default(),
            aggregate: RwLock::default(),
            members: Mutex::default(),
            revision: AtomicU64::new(0),
        };
        registry.attach(session, gate);
        registry
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub(crate) fn attach(&self, session: &Arc<SessionContext>, gate: &Arc<StatementGate>) {
        let mut members = self.members.lock();
        members
            .sessions
            .retain(|entry| entry.context.strong_count() != 0 && entry.gate.strong_count() != 0);
        members.sessions.push(Session {
            context: Arc::downgrade(session),
            gate: Arc::downgrade(gate),
        });
    }

    /// Freeze publication and every existing session gate before invalidating
    /// plans. No shared registry lock is added to the query hot path. New
    /// sessions attach under the same membership lock before becoming public.
    fn change<T>(&self, change: impl FnOnce(&mut Members) -> Result<T>) -> Result<T> {
        // Keep even replaced callbacks alive while invalidation drops plans.
        // Host destructors run only after all registry/session locks are gone.
        let mut retained = Vec::new();
        let mut members = self
            .members
            .try_lock()
            .ok_or(SQLFunctionRegistrationError::Busy)?;
        let sessions: Vec<_> = members
            .sessions
            .iter()
            .filter_map(|member| Some((member.context.upgrade()?, member.gate.upgrade()?)))
            .collect();
        let _gates: Vec<_> = sessions
            .iter()
            .map(|(_, gate)| {
                gate.try_lock_idle()
                    .ok_or(SQLFunctionRegistrationError::Busy)
            })
            .collect::<Result<_>>()?;
        members.functions.retain(|function| {
            if let Some(function) = function.upgrade() {
                retained.push(function);
                true
            } else {
                false
            }
        });
        let next_revision = self
            .revision()
            .checked_add(1)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "54000".into(),
                message: "SQL function registry revision space exhausted".into(),
            })?;
        for (session, _) in &sessions {
            session.state.write().sql_statement_cache.clear();
            uqa_execution::statement::prepared::invalidation::PreparedCatalogChange::GlobalCatalog
                .invalidate_with_routines(
                    session.prepared.write().values_mut(),
                    &session.routine_bodies,
                );
        }
        let result = change(&mut members);
        if result.is_ok() {
            self.revision.store(next_revision, Ordering::Release);
        }
        result
    }
}

/// An identity-specific lease on a shared registration, independent of the
/// creating session. Dropping this handle releases the lease without removing
/// its map entry. Call `unregister` to retire that entry explicitly.
pub struct SQLFunctionRegistration {
    active: Option<Registration>,
}

struct Registration {
    registry: Arc<Registry>,
    name: String,
    function: Function,
}

macro_rules! registration {
    ($method:ident, $field:ident, $kind:ident, $trait:ident) => {
        impl Registry {
            pub(super) fn $method(
                self: &Arc<Self>,
                name: String,
                function: Arc<dyn $trait>,
                options: crate::SQLFunctionOptions,
            ) -> Result<SQLFunctionRegistration> {
                let previous = self.change(|members| {
                    members
                        .functions
                        .push(WeakFunction::$kind(Arc::downgrade(&function)));
                    Ok(self.$field.write().insert(
                        name.clone(),
                        RegisteredSQLFunction::new(Arc::clone(&function), options),
                    ))
                })?;
                drop(previous);
                Ok(SQLFunctionRegistration {
                    active: Some(Registration {
                        registry: Arc::clone(self),
                        name,
                        function: Function::$kind(function),
                    }),
                })
            }
        }
    };
}
registration!(register_scalar, scalar, Scalar, SQLScalarFunction);
registration!(register_table, table, Table, SQLTableFunction);
registration!(
    register_aggregate,
    aggregate,
    Aggregate,
    SQLAggregateFactory
);

impl SQLFunctionRegistration {
    /// Prevent future calls and release this registration. Busy preserves the
    /// handle and its map entry. An older handle never removes a replacement.
    /// Success releases the shared registry lease as well as the function.
    /// Repeated successful retirement is a no-op.
    pub fn unregister(&mut self) -> Result<()> {
        let Some(active) = &self.active else {
            return Ok(());
        };
        active.registry.change(|_| match &active.function {
            Function::Scalar(function) => retire(&active.registry.scalar, &active.name, function),
            Function::Table(function) => retire(&active.registry.table, &active.name, function),
            Function::Aggregate(function) => {
                retire(&active.registry.aggregate, &active.name, function)
            }
        })?;
        self.active.take();
        Ok(())
    }
}

fn retire<F: ?Sized>(map: &Map<F>, name: &str, function: &Arc<F>) -> Result<()> {
    let mut map = map.write();
    let current = map
        .get(name)
        .is_some_and(|entry| Arc::ptr_eq(&entry.function, function));
    // This handle, change's temporary destructor hold, and optionally the map.
    if Arc::strong_count(function) != 2 + usize::from(current) {
        return Err(SQLFunctionRegistrationError::Busy);
    }
    if current {
        map.remove(name);
    }
    Ok(())
}
