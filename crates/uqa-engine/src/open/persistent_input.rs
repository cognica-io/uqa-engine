//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A private restore session retains its transaction until complete catalog validation.

use super::{table_input::PersistentTableInput, CatalogRestoreMode, Engine};
use std::sync::{atomic::Ordering, Arc};
use uqa_core::memory::{Budgeted, MemoryBudget};
use uqa_storage::{
    catalog::input::CatalogInputLimits, key_value::KeyValueReadRevision,
    read_control::StorageReadControl, CatalogCacheRevisions, PersistentStorageProvider,
    StorageBackendError, StorageBackendResult,
};

pub(crate) struct PersistentEngineInput {
    candidate: Option<Candidate>,
    control: StorageReadControl,
}

struct Candidate {
    session: PersistentEngineState,
    stage: Stage,
    limits: CatalogInputLimits,
}

enum Stage {
    Metadata,
    Roles(uqa_execution::catalog::security::roles::persistence::input::RoleCatalogInput),
    Schemas(uqa_execution::schema::namespaces::restoration::input::SchemaCatalogInput),
    Tables(PersistentTableInput),
    Registries,
    Caches,
}

/// The complete private catalog still owns its read transaction. Closing or dropping this owner releases that transaction before the provider can publish its files.
pub(crate) struct PersistentEngineState {
    engine: Budgeted<Box<Engine>>,
    version: Option<u64>,
    read_view: Option<KeyValueReadRevision>,
    revisions: Option<CatalogCacheRevisions>,
}

impl PersistentEngineInput {
    pub(crate) fn new(
        provider: Arc<dyn PersistentStorageProvider>,
        retention: MemoryBudget,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let memory = control.memory().reserve(size_of::<Engine>())?;
        let storage = provider.open_session()?;
        storage.validate_transaction_affinity()?;
        let engine = Budgeted::new(
            Box::new(Engine::empty_persistent_session_with_retention(
                storage,
                Some(provider),
                retention,
            )),
            memory,
        );
        let backend = engine.storage.backend.as_ref().expect("persistent backend");
        let version = backend.change_version()?;
        backend.begin_read_transaction()?;
        backend.pin_transaction_snapshot()?;
        let read_view = backend.read_view_revision()?;
        let revisions = engine
            .storage
            .catalog
            .as_ref()
            .expect("persistent catalog")
            .cache_revisions()?;
        Ok(Self {
            candidate: Some(Candidate {
                session: PersistentEngineState {
                    engine,
                    version,
                    read_view,
                    revisions,
                },
                stage: Stage::Metadata,
                limits,
            }),
            control: control.clone(),
        })
    }

    /// Role, schema and table input consume bounded native polls. The remaining metadata, registry and model owners retain their existing whole-operation boundaries. Any error or unwind consumes the private session, including its read transaction.
    pub(crate) fn advance(
        &mut self,
        maximum_steps: usize,
    ) -> StorageBackendResult<Option<PersistentEngineState>> {
        let mut candidate = self.candidate.take().ok_or_else(|| {
            StorageBackendError::Other("persistent Engine input is finished".into())
        })?;
        if maximum_steps == 0 {
            return Err(StorageBackendError::Other(
                "zero persistent Engine input window".into(),
            ));
        }
        for _ in 0..maximum_steps {
            self.control.check()?;
            let engine = candidate.session.engine.as_mut();
            let catalog = Arc::clone(engine.storage.catalog.as_ref().expect("persistent catalog"));
            candidate.stage = match candidate.stage {
                Stage::Metadata => {
                    uqa_execution::schema::constraints::restoration::validate_constraint_catalog(
                        catalog.as_ref(),
                    )?;
                    Stage::Roles(uqa_execution::catalog::security::roles::persistence::input::RoleCatalogInput::new(
                        catalog,
                        candidate.limits,
                        engine.durable.roles.read().budget(),
                        &self.control,
                    )?)
                }
                Stage::Roles(mut input) => match input.advance(1)? {
                    Some(values) => {
                        *engine.durable.roles.write() = values.roles;
                        *engine.durable.role_memberships.write() = values.memberships;
                        Stage::Schemas(uqa_execution::schema::namespaces::restoration::input::SchemaCatalogInput::new(
                            catalog,
                            engine.durable.roles.read().clone(),
                            candidate.limits,
                            engine.durable.schemas.read().budget(),
                            &self.control,
                        )?)
                    }
                    None => Stage::Roles(input),
                },
                Stage::Schemas(mut input) => match input.advance(1)? {
                    Some(schemas) => {
                        *engine.durable.schemas.write() = schemas;
                        Stage::Tables(PersistentTableInput::from_catalog(
                            catalog,
                            Arc::clone(
                                engine.storage.backend.as_ref().expect("persistent backend"),
                            ),
                            engine.durable.roles.read().clone(),
                            candidate.limits,
                            &self.control,
                        )?)
                    }
                    None => Stage::Schemas(input),
                },
                Stage::Tables(mut input) => match input.advance(1)? {
                    Some(tables) => {
                        engine.storage.tables = Arc::new(tables);
                        Stage::Registries
                    }
                    None => Stage::Tables(input),
                },
                Stage::Registries => {
                    engine.synchronize_partition_identity_watermarks()?;
                    engine.restore_graphs_from_catalog(catalog.as_ref())?;
                    engine.restore_engine_registries_from_catalog(
                        catalog.as_ref(),
                        CatalogRestoreMode::LoadOnly,
                    )?;
                    uqa_execution::schema::types::relation_arrays::restoration::validate(
                        &engine.restored_catalog_read_view(),
                    )?;
                    Stage::Caches
                }
                Stage::Caches => {
                    engine.restore_model_caches(catalog.as_ref())?;
                    self.control.check()?;
                    return Ok(Some(candidate.session));
                }
            };
        }
        self.candidate = Some(candidate);
        Ok(None)
    }
}

impl PersistentEngineState {
    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    pub(crate) fn finish(self) -> StorageBackendResult<Engine> {
        let engine = &self.engine;
        let catalog = engine.storage.catalog.as_ref().expect("persistent catalog");
        let backend = engine.storage.backend.as_ref().expect("persistent backend");
        let revisions = catalog.cache_revisions()?;
        if self.revisions == revisions {
            *engine.epochs.storage_cache_revisions.lock() = revisions;
            let read_view = backend.read_view_revision()?;
            if self.read_view == read_view {
                *engine.epochs.seen_storage_read_view.lock() = read_view;
            }
            if let Some(version) = backend.change_version()? {
                engine
                    .epochs
                    .seen_storage_change_version
                    .store(version, Ordering::Release);
            }
        }
        backend.rollback_transaction()?;
        if backend.change_version()? != self.version {
            engine
                .epochs
                .seen_storage_change_version
                .store(self.version.unwrap_or_default(), Ordering::Release);
            *engine.epochs.seen_storage_read_view.lock() = None;
        }
        let (engine, _memory) = self.engine.into_parts();
        Ok(*engine)
    }
}

impl Engine {
    pub(crate) fn from_restored_persistent_provider(
        provider: &Arc<dyn PersistentStorageProvider>,
        retention: MemoryBudget,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut input =
            PersistentEngineInput::new(Arc::clone(provider), retention, limits, control)?;
        let mut engine = loop {
            if let Some(session) = input.advance(64)? {
                break session.finish()?;
            }
        };
        engine.activate_persistent_provider(provider)?;
        Ok(engine)
    }
}

#[cfg(test)]
mod tests;
