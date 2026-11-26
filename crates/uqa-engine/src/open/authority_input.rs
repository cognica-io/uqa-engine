//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private authority generations retain their original position around routine placeholders.

use super::Engine;
use std::sync::Arc;
use uqa_execution::{
    catalog::{
        domain::DomainRestoreState, security::builtin_routines::input::BuiltinRoutineCatalogInput,
        security::database_lifecycle::input::DatabaseCatalogInput,
        security::system_relations::input::SystemRelationCatalogInput,
    },
    routines::{
        catalog::input::RoutineCatalogPlaceholderInput, restoration::RetainedRoutineFinalization,
    },
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, StorageBackendResult,
};

pub(super) struct RestoredAuthorities {
    pub domains: DomainRestoreState,
    pub routines: RetainedRoutineFinalization,
}

pub(super) enum PersistentAuthorityInput {
    Begin(DomainRestoreState),
    System {
        input: SystemRelationCatalogInput,
        domains: DomainRestoreState,
    },
    Database {
        input: DatabaseCatalogInput,
        domains: DomainRestoreState,
    },
    DefaultAcls {
        input: uqa_execution::catalog::security::default_acl::persistence::DefaultAclInput,
        domains: DomainRestoreState,
    },
    Routines {
        input: RoutineCatalogPlaceholderInput,
        domains: DomainRestoreState,
    },
    Builtins {
        input: BuiltinRoutineCatalogInput,
        retained: RestoredAuthorities,
    },
}

pub(super) enum AuthorityProgress {
    Pending(PersistentAuthorityInput),
    Complete(RestoredAuthorities),
}

impl PersistentAuthorityInput {
    pub(super) fn new(domains: DomainRestoreState) -> Self {
        Self::Begin(domains)
    }

    /// The owning Engine candidate retires its complete private session on any failure or unwind. Later registry compilation retains domain completion and pending routine definitions.
    pub(super) fn advance(
        self,
        engine: &Engine,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<AuthorityProgress> {
        control.check()?;
        let catalog = engine.storage.catalog.as_ref().expect("persistent catalog");
        let next = match self {
            Self::Begin(domains) => Self::System {
                input: SystemRelationCatalogInput::new(
                    Arc::clone(catalog),
                    engine.durable.roles.read().clone(),
                    limits,
                    engine.durable.system_relation_security.read().budget(),
                    control,
                )?,
                domains,
            },
            Self::System { mut input, domains } => match input.advance(1)? {
                None => Self::System { input, domains },
                Some(value) => {
                    *engine.durable.system_relation_security.write() = value;
                    Self::Database {
                        input: DatabaseCatalogInput::new(
                            Arc::clone(catalog),
                            engine.durable.roles.read().clone(),
                            limits,
                            engine.durable.database_security.read().budget(),
                            control,
                        )?,
                        domains,
                    }
                }
            },
            Self::Database { mut input, domains } => match input.advance(1)? {
                Some(value) => {
                    *engine.durable.database_security.write() = value;
                    Self::DefaultAcls {
                        input: uqa_execution::catalog::security::default_acl::persistence::DefaultAclInput::new(Arc::clone(catalog), engine.durable.roles.read().clone(), limits, engine.durable.default_acls.read().budget(), control)?,
                        domains,
                    }
                }
                None => Self::Database { input, domains },
            },
            Self::DefaultAcls { mut input, domains } => match input.advance(1)? {
                None => Self::DefaultAcls { input, domains },
                Some(defaults) => {
                    engine
                        .durable
                        .default_acls
                        .write()
                        .replace_durable(defaults);
                    Self::Routines {
                        input: engine.routine_placeholder_input(limits, control)?,
                        domains,
                    }
                }
            },
            Self::Routines { mut input, domains } => match input.advance(1)? {
                None => Self::Routines { input, domains },
                Some(prepared) => {
                    let routines = RetainedRoutineFinalization::install(
                        engine,
                        prepared,
                        control,
                        engine.extensions.functions.clone(),
                    )?;
                    Self::Builtins {
                        input: BuiltinRoutineCatalogInput::new(
                            Arc::clone(catalog),
                            engine.durable.roles.read().clone(),
                            limits,
                            engine.durable.builtin_routine_security.read().budget(),
                            control,
                        )?,
                        retained: RestoredAuthorities { domains, routines },
                    }
                }
            },
            Self::Builtins {
                mut input,
                retained,
            } => match input.advance(1)? {
                Some(values) => {
                    *engine.durable.builtin_routine_security.write() = values;
                    return Ok(AuthorityProgress::Complete(retained));
                }
                None => Self::Builtins { input, retained },
            },
        };
        Ok(AuthorityProgress::Pending(next))
    }
}
