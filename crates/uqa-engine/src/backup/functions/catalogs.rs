//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql_error, StoredFunctionSite};
use crate::{Engine, SQLError};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, Produced, ProductionControl},
};
use uqa_execution::catalog::domain::{read_stored_domain_declaration, DOMAIN_RECORD_PREFIX};
use uqa_sql::{
    binding::view_dependencies::{
        visit_stored_query_json_runtime_calls, visit_stored_runtime_calls_from,
    },
    catalog::{
        domain::StoredDomain, stored_view::CapturedViewDefinitions,
        type_definitions::CapturedTypeDefinitions,
    },
    plan::PlanRef,
};
use uqa_storage::{
    backup::{
        BackupCatalogTextInput, BackupError, BackupImageSource, BackupResult,
        BackupSourceStatistics,
    },
    catalog::input::{CatalogInputLimits, CatalogInputStep},
    memory_capture::CapturedMemoryValue,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub(in crate::backup) enum Catalogs {
    Native {
        views: Native<CapturedViewDefinitions>,
        domains: Native<CapturedTypeDefinitions<StoredDomain>>,
    },
    Provider {
        views: BackupCatalogTextInput,
        domains: BackupCatalogTextInput,
    },
}

pub(in crate::backup) enum Native<T> {
    Memory(Arc<Budgeted<CapturedMemoryValue<T>>>),
    Staged(Produced<T>),
}

impl<T> Native<T> {
    fn get(&self) -> StorageBackendResult<&T> {
        match self {
            Self::Memory(cut) => cut.get(),
            Self::Staged(root) => Ok(root),
        }
    }
}

impl Catalogs {
    pub(in crate::backup) fn memory(cut: &crate::state::SnapshotCut) -> StorageBackendResult<Self> {
        Ok(Self::Native {
            views: Native::Memory(cut.catalogs.views.captured(&cut.cut)?),
            domains: Native::Memory(cut.catalogs.domains.captured(&cut.cut)?),
        })
    }

    pub(super) fn staged(
        engine: &Engine,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self::Native {
            views: Native::Staged(engine.durable.views.read().capture(control)?),
            domains: Native::Staged(engine.durable.domains.read().capture(control)?),
        })
    }

    pub(in crate::backup) fn provider(
        image: &impl BackupImageSource,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Option<Self>> {
        let views = image.stored_view_queries(limits, control)?;
        let domains = image.stored_metadata_values(DOMAIN_RECORD_PREFIX, limits, control)?;
        match (views, domains) {
            (Some(views), Some(domains)) => Ok(Some(Self::Provider { views, domains })),
            (None, None) => Ok(None),
            _ => Err(BackupError::Unsupported(
                "incomplete catalog function-reference projection",
            )),
        }
    }

    pub(super) async fn visit(
        self,
        source: &StorageReadControl,
        work: &mut AsyncImageInput<'_>,
        counts: &Mutex<BackupSourceStatistics>,
        visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
    ) -> StorageBackendResult<()> {
        let production =
            ProductionControl::new(source.memory(), source.cancellation(), work.cancellation);
        match self {
            Self::Native { views, domains } => {
                for (_, view) in views.get()? {
                    work.checkpoint().await?;
                    counts.lock().visited_positions += 1;
                    visit_stored_runtime_calls_from(
                        PlanRef::Query(&view.query),
                        &production,
                        work,
                        visit,
                    )
                    .await
                    .map_err(sql_error)?;
                }
                for (_, domain) in domains.get()? {
                    work.checkpoint().await?;
                    counts.lock().visited_positions += 1;
                    domain
                        .definition
                        .visit_runtime_scalar_calls(source.cancellation(), work, visit)
                        .await
                        .map_err(sql_error)?;
                }
            }
            Self::Provider { views, domains } => {
                for (kind, mut records) in
                    [(Definition::View, views), (Definition::Domain, domains)]
                {
                    let before = *counts.lock();
                    loop {
                        work.checkpoint().await?;
                        let step = records.advance().map_err(source_error)?;
                        *counts.lock() = before.saturating_add(records.statistics());
                        match step {
                            CatalogInputStep::Complete => break,
                            CatalogInputStep::Pending => (),
                            CatalogInputStep::Row(bytes) => match kind {
                                Definition::View => visit_stored_query_json_runtime_calls(
                                    &bytes,
                                    &production,
                                    work,
                                    visit,
                                )
                                .await
                                .map_err(sql_error)?,
                                Definition::Domain => {
                                    let declaration =
                                        read_stored_domain_declaration(&bytes, source, work)
                                            .await?;
                                    declaration
                                        .visit_runtime_scalar_calls(
                                            source.cancellation(),
                                            work,
                                            visit,
                                        )
                                        .await
                                        .map_err(sql_error)?;
                                }
                            },
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

enum Definition {
    View,
    Domain,
}

fn source_error(error: BackupError) -> StorageBackendError {
    match error {
        BackupError::Storage(error) => error,
        BackupError::Cancelled(error) => error.into(),
        BackupError::Memory(error) => error.into(),
        other => StorageBackendError::backend("stored catalog source", other),
    }
}
