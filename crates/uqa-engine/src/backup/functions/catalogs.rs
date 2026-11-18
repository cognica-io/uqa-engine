//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{routines, sql_error, tables::Tables, StoredFunctionSite};
use crate::{Engine, SQLError};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, Produced, ProductionControl},
};
use uqa_execution::catalog::{
    domain::{read_stored_domain_declaration, DOMAIN_RECORD_PREFIX},
    table_capture::visit_stored_table_functions,
};
use uqa_execution::schema::events::persistence::{
    visit_stored_rule_runtime_calls, visit_stored_trigger_runtime_calls, RULES_METADATA_KEY,
    TRIGGERS_METADATA_KEY,
};
use uqa_sql::{
    binding::view_dependencies::{
        visit_stored_query_json_runtime_calls, visit_stored_runtime_calls_from,
    },
    catalog::{
        domain::StoredDomain,
        events::{CapturedEventCatalog, EventRuntimeCalls, StoredRule, StoredTrigger},
        stored_view::CapturedViewDefinitions,
        type_definitions::CapturedTypeDefinitions,
    },
    plan::PlanRef,
    routines::registry::CapturedRoutines,
};
use uqa_storage::{
    backup::{
        BackupCatalogField, BackupCatalogTextInput, BackupError, BackupImageSource, BackupResult,
        BackupSourceStatistics, BackupTableField,
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
        tables: Tables,
        routines: Native<CapturedRoutines>,
        triggers: Native<CapturedEventCatalog<StoredTrigger>>,
        rules: Native<CapturedEventCatalog<StoredRule>>,
    },
    Provider {
        views: BackupCatalogTextInput,
        domains: BackupCatalogTextInput,
        tables: [BackupCatalogTextInput; 2],
        routines: BackupCatalogTextInput,
        events: [BackupCatalogTextInput; 2],
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
            tables: Tables::Memory(cut.tables.clone()),
            routines: Native::Memory(cut.catalogs.sql_user_functions.captured(&cut.cut)?),
            triggers: Native::Memory(cut.catalogs.triggers.captured(&cut.cut)?),
            rules: Native::Memory(cut.catalogs.rules.captured(&cut.cut)?),
        })
    }

    pub(super) fn staged(
        engine: &Engine,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self::Native {
            views: Native::Staged(engine.durable.views.read().capture(control)?),
            domains: Native::Staged(engine.durable.domains.read().capture(control)?),
            tables: Tables::Staged(engine.storage.tables.live_snapshot()),
            routines: Native::Staged(engine.durable.sql_user_functions.read().capture(control)?),
            triggers: Native::Staged(engine.durable.triggers.read().capture(control)?),
            rules: Native::Staged(engine.durable.rules.read().capture(control)?),
        })
    }

    pub(in crate::backup) fn provider(
        image: &impl BackupImageSource,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Option<Self>> {
        let open = |field| image.stored_catalog_text(field, limits, control);
        let Some(views) = open(BackupCatalogField::ViewQuery)? else {
            return Ok(None);
        };
        let required = |field| {
            open(field)?.ok_or(BackupError::Unsupported(
                "incomplete catalog function-reference projection",
            ))
        };
        Ok(Some(Self::Provider {
            views,
            domains: required(BackupCatalogField::MetadataPrefix(DOMAIN_RECORD_PREFIX))?,
            tables: [
                required(BackupCatalogField::Table(BackupTableField::Columns))?,
                required(BackupCatalogField::Table(BackupTableField::Constraints))?,
            ],
            routines: required(BackupCatalogField::Metadata(
                uqa_execution::routines::catalog::FUNCTIONS_METADATA_KEY,
            ))?,
            events: [
                required(BackupCatalogField::Metadata(TRIGGERS_METADATA_KEY))?,
                required(BackupCatalogField::Metadata(RULES_METADATA_KEY))?,
            ],
        }))
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
            Self::Native {
                views,
                domains,
                tables,
                routines,
                triggers,
                rules,
            } => {
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
                tables.visit(source, work, counts, visit).await?;
                routines::native(routines.get()?, source, work, counts, visit).await?;
                visit_events(triggers.get()?, source, work, counts, visit).await?;
                visit_events(rules.get()?, source, work, counts, visit).await?;
            }
            Self::Provider {
                views,
                domains,
                tables,
                routines,
                events: [triggers, rules],
            } => {
                let [columns, checks] = tables;
                for (kind, mut records) in [
                    (Definition::View, views),
                    (Definition::Domain, domains),
                    (Definition::Table(BackupTableField::Columns), columns),
                    (Definition::Table(BackupTableField::Constraints), checks),
                    (Definition::Routines, routines),
                    (Definition::Triggers, triggers),
                    (Definition::Rules, rules),
                ] {
                    let before = *counts.lock();
                    loop {
                        work.checkpoint().await?;
                        let step = records.advance().map_err(source_error)?;
                        *counts.lock() = before.saturating_add(records.statistics());
                        match step {
                            CatalogInputStep::Complete => break,
                            CatalogInputStep::Pending => (),
                            CatalogInputStep::Row(bytes) => match kind {
                                Definition::Routines => {
                                    routines::record(&bytes, source, work, visit).await?;
                                }
                                Definition::Triggers => {
                                    visit_stored_trigger_runtime_calls(&bytes, source, work, visit)
                                        .await?;
                                }
                                Definition::Rules => {
                                    visit_stored_rule_runtime_calls(&bytes, source, work, visit)
                                        .await?;
                                }
                                Definition::Table(field) => {
                                    visit_stored_table_functions(
                                        &bytes, field, source, work, visit,
                                    )
                                    .await?;
                                }
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
    Table(BackupTableField),
    Routines,
    Triggers,
    Rules,
}

async fn visit_events<T: EventRuntimeCalls>(
    catalog: &CapturedEventCatalog<T>,
    source: &StorageReadControl,
    work: &mut AsyncImageInput<'_>,
    counts: &Mutex<BackupSourceStatistics>,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> StorageBackendResult<()> {
    for (_, entries) in catalog {
        work.checkpoint().await?;
        for entry in entries.values() {
            work.checkpoint().await?;
            counts.lock().visited_positions += 1;
            entry
                .runtime_definition()
                .visit_runtime_calls(source.cancellation(), work, visit)
                .await
                .map_err(sql_error)?;
        }
    }
    Ok(())
}

fn source_error(error: BackupError) -> StorageBackendError {
    match error {
        BackupError::Storage(error) => error,
        BackupError::Cancelled(error) => error.into(),
        BackupError::Memory(error) => error.into(),
        other => StorageBackendError::backend("stored catalog source", other),
    }
}
