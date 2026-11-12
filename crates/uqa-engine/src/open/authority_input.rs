//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private authority generations retain their original position around routine placeholders.

use super::{CatalogRestoreMode, Engine};
use std::sync::Arc;
use uqa_execution::{
    catalog::{
        domain::DomainRestoreState, security::builtin_routines::input::BuiltinRoutineCatalogInput,
    },
    routines::restoration::PendingSQLFunctionRestore,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, StorageBackendResult,
};

pub(super) struct RestoredAuthorities {
    pub domains: DomainRestoreState,
    pub routines: Option<PendingSQLFunctionRestore>,
}

pub(super) enum PersistentAuthorityInput {
    System(DomainRestoreState),
    Database(DomainRestoreState),
    Routines(DomainRestoreState),
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
        Self::System(domains)
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
            Self::System(domains) => {
                *engine.durable.system_relation_security.write() =
                    uqa_execution::catalog::security::system_relations::restore(
                        catalog.as_ref(),
                        &engine.durable.roles.read(),
                        false,
                    )?;
                Self::Database(domains)
            }
            Self::Database(domains) => {
                engine.restore_database_security_from_metadata(catalog.as_ref(), false)?;
                Self::Routines(domains)
            }
            Self::Routines(domains) => {
                let routines = engine.install_sql_function_restore_placeholders(
                    catalog.as_ref(),
                    CatalogRestoreMode::LoadOnly,
                )?;
                let input = BuiltinRoutineCatalogInput::new(
                    Arc::clone(catalog),
                    engine.durable.roles.read().clone(),
                    limits,
                    engine.durable.builtin_routine_security.read().budget(),
                    control,
                )?;
                Self::Builtins {
                    input,
                    retained: RestoredAuthorities { domains, routines },
                }
            }
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
