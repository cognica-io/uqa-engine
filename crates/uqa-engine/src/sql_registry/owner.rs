//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared registry publication, session invalidation and registration retirement.

use super::declarations::{CapturedDeclarations, Declarations, RegisteredKind};
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
    declarations: Declarations,
    sessions: Vec<Session>,
    functions: Vec<WeakFunction>,
    restore_readers: Weak<()>,
}

pub(crate) struct Registry {
    pub(crate) scalar: Map<dyn SQLScalarFunction>,
    pub(crate) table: Map<dyn SQLTableFunction>,
    pub(crate) aggregate: Map<dyn SQLAggregateFactory>,
    members: Mutex<Members>,
    catalog_publication: RwLock<()>,
    revision: AtomicU64,
}

impl Registry {
    pub(crate) fn new(session: &Arc<SessionContext>, gate: &Arc<StatementGate>) -> Self {
        let registry = Self {
            scalar: RwLock::default(),
            table: RwLock::default(),
            aggregate: RwLock::default(),
            members: Mutex::default(),
            catalog_publication: RwLock::default(),
            revision: AtomicU64::new(0),
        };
        registry.attach(session, gate);
        registry
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Pin the installed declarations while a private maintenance session binds its stored catalog. The session never submits application statements or owns a foreground registration gate.
    pub(crate) fn with_catalog_declarations<T>(&self, read: impl FnOnce() -> T) -> T {
        let _declarations = self.catalog_publication.read();
        read()
    }

    pub(crate) fn kinds(&self, name: &str) -> super::RegisteredKinds {
        self.members.lock().declarations.get(name)
    }

    /// Retain declaration names at this statement's captured cut. A later installation or retirement can proceed while a backup is reading its catalog.
    pub(crate) fn capture_declarations(
        &self,
        memory: &uqa_core::memory::MemoryBudget,
    ) -> Result<CapturedDeclarations> {
        Ok(self.members.lock().declarations.capture(memory)?)
    }

    /// Keep registered names and implementations fixed while a restore prepares and publishes its private database.
    pub(crate) fn retain_for_restore(&self) -> Arc<()> {
        let mut members = self.members.lock();
        if let Some(active) = members.restore_readers.upgrade() {
            return active;
        }
        let active = Arc::new(());
        members.restore_readers = Arc::downgrade(&active);
        active
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
        let _publication = self.catalog_publication.write();
        let mut members = self
            .members
            .try_lock()
            .ok_or(SQLFunctionRegistrationError::Busy)?;
        if members.restore_readers.strong_count() != 0 {
            return Err(SQLFunctionRegistrationError::Busy);
        }
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
                        .declarations
                        .set(&name, RegisteredKind::$kind, true)?;
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
        active.registry.change(|members| match &active.function {
            Function::Scalar(function) => retire(
                &active.registry.scalar,
                &active.name,
                function,
                &mut members.declarations,
                RegisteredKind::Scalar,
            ),
            Function::Table(function) => retire(
                &active.registry.table,
                &active.name,
                function,
                &mut members.declarations,
                RegisteredKind::Table,
            ),
            Function::Aggregate(function) => retire(
                &active.registry.aggregate,
                &active.name,
                function,
                &mut members.declarations,
                RegisteredKind::Aggregate,
            ),
        })?;
        self.active.take();
        Ok(())
    }
}

fn retire<F: ?Sized>(
    map: &Map<F>,
    name: &str,
    function: &Arc<F>,
    declarations: &mut Declarations,
    kind: RegisteredKind,
) -> Result<()> {
    let mut map = map.write();
    let current = map
        .get(name)
        .is_some_and(|entry| Arc::ptr_eq(&entry.function, function));
    // This handle, change's temporary destructor hold, and optionally the map.
    if Arc::strong_count(function) != 2 + usize::from(current) {
        return Err(SQLFunctionRegistrationError::Busy);
    }
    if current {
        declarations.set(name, kind, false)?;
        map.remove(name);
    }
    Ok(())
}
