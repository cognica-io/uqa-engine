//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained persistent fields enter the ordinary table state and lookup registry.

use crate::state::{TableContents, TableRegistry, TableRegistryInput};
use std::sync::Arc;
use uqa_analysis::{AnalyzerDefinition, AnalyzerResources};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder, ImageRead},
    memory::{MemoryReservation, ProductionControl},
};
use uqa_execution::{
    catalog::security::relation_restoration::{
        PreparedTableDeclaration, PreparedTableDeclarations,
    },
    schema::indexes::constraint_names::KeyConstraintNames,
};
use uqa_sql::catalog::table_metadata::TableMetadata;
use uqa_storage::{
    catalog::input::{CatalogInputLimits, CatalogInputStep},
    document_store::{identifiers, maximum::MaximumDocIdStep},
    inverted_index::AnalyzerDefault,
    read_control::StorageReadControl,
    statistics_maintenance::StatisticsMaintenance,
    vector_index::PersistentVectorIndexesBuilder,
    CatalogFacade, PersistentStorageBackend, StorageBackendError, StorageBackendResult,
    StorageName,
};

pub(crate) struct PersistentTableInput {
    input: ImageDecoder<TableRegistry, StorageBackendError>,
}

struct Declarations {
    tables: std::vec::IntoIter<PreparedTableDeclaration>,
    _memory: MemoryReservation,
}

impl super::Engine {
    pub(super) fn table_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PersistentTableInput> {
        PersistentTableInput::from_catalog(
            Arc::clone(self.storage.catalog.as_ref().expect("persistent catalog")),
            Arc::clone(self.storage.backend.as_ref().expect("persistent backend")),
            self.durable.roles.read().clone(),
            limits,
            control,
        )
    }
}

impl PersistentTableInput {
    /// Restore complete table membership from one caller-retained catalog transaction. Physical table/ACL validation and all columns/authority precede index-name input, which precedes table hydration. Failure never publishes an earlier table prefix.
    pub(crate) fn from_catalog(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
        roles: uqa_sql::catalog::roles::RoleDefinitions,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut tables = catalog.begin_table_load(limits, &retained)?;
                let tables = loop {
                    reader.input().checkpoint().await?;
                    if let Some(tables) = tables.advance(1)? {
                        break tables;
                    }
                };
                let mut declarations = uqa_execution::catalog::security::relation_restoration::TableDeclarationInput::new(tables, roles, &retained)?;
                let declarations = loop {
                    reader.input().checkpoint().await?;
                    if let Some(declarations) = declarations.advance(1)? {
                        break declarations;
                    }
                };
                let names = KeyConstraintNames::load_from(
                    &*catalog,
                    limits,
                    &retained,
                    &mut reader.input(),
                )
                .await?;
                let mut hydration =
                    Self::new(catalog, backend, declarations, names, limits, &retained)?;
                loop {
                    reader.input().checkpoint().await?;
                    if let Some(tables) = hydration.advance(1)? {
                        break Ok(tables);
                    }
                }
            },
        )?;
        Ok(Self { input })
    }

    /// The caller retains one transaction view for the paired catalog and backend. Complete column/authority preparation and index-owned key names precede this stage. Only a complete registry is published; no physical writes or resource resolution occur.
    pub(crate) fn new(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
        declarations: PreparedTableDeclarations,
        names: KeyConstraintNames,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        if !declarations.budget().shares_allowance(control.memory()) {
            return Err(StorageBackendError::Other(
                "persistent table declarations require their original allowance".into(),
            ));
        }
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let (tables, memory) = declarations.into_parts();
                let mut source = Declarations {
                    tables: tables.into_iter(),
                    _memory: memory,
                };
                let mut registry = TableRegistryInput::new(None, &retained)?;
                for declaration in source.tables.by_ref() {
                    reader.input().checkpoint().await?;
                    let (name, table) = hydrate(
                        &*catalog,
                        &backend,
                        declaration,
                        &names,
                        limits,
                        &retained,
                        &mut reader,
                    )
                    .await?;
                    registry = registry.add_table(name, table)?;
                }
                drop(source);
                registry.finish()
            },
        )?;
        Ok(Self { input })
    }

    /// Each advance bounds continuation polls. Physical records retain their independent encoded limits; long source populations and nested decoders consume the same work channel. Failure consumes the complete unpublished registry and unread declarations.
    pub(crate) fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<TableRegistry>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn hydrate(
    catalog: &dyn CatalogFacade,
    backend: &Arc<dyn PersistentStorageBackend>,
    declaration: PreparedTableDeclaration,
    names: &KeyConstraintNames,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    reader: &mut ImageRead,
) -> StorageBackendResult<(
    uqa_core::memory::Budgeted<uqa_core::RelationIdentity>,
    Arc<crate::TableState>,
)> {
    let PreparedTableDeclaration {
        relation,
        security,
        object_id,
        storage_generation,
        analyzer_json,
        fts_fields,
        vector_fields,
        columns,
        constraints_json,
    } = declaration;
    let name =
        StorageName::from_admitted(relation.qualified_name_from(&mut reader.input()).await?)?;
    let analyzer = AnalyzerDefinition::from_stored_from(analyzer_json, reader.input())
        .await
        .map_err(|error| StorageBackendError::backend("stored table analyzer", error))?;
    let default =
        AnalyzerDefault::from_definition(analyzer.clone(), AnalyzerResources::default(), control)?;
    let documents = backend.document_store_with_control(name.clone(), control)?;
    let text = backend.inverted_index_with_control(name.clone(), default, control)?;
    let mut vector_input = PersistentVectorIndexesBuilder::catalog_input(
        backend.clone(),
        name.clone(),
        vector_fields,
        control,
    )?;
    let vectors = read_vectors(&mut vector_input, &mut reader.input()).await?;
    drop(vector_input);
    reader.input().checkpoint().await?;
    let constraints = names.decode_stored_fields_with_control(
        &relation,
        object_id,
        &constraints_json,
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )?;
    drop(constraints_json);
    if constraints.catalog_oids.is_some_and(|oids| {
        !oids.is_valid_for(
            uqa_sql::catalog::relation_oids::RelationOidKind::Table,
            &object_id,
        )
    }) {
        return Err(StorageBackendError::Other(format!(
            "table `{name}` records invalid catalog OIDs"
        )));
    }
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    uqa_sql::schema::constraint_metadata::identity::validate_not_null_identities_with_control(
        &columns,
        &production,
    )
    .map_err(|error| StorageBackendError::backend("stored NOT NULL identities", error))?;
    uqa_sql::schema::constraint_metadata::identity::foreign_keys::validate_with_control(
        &columns,
        &constraints,
        &production,
    )
    .map_err(|error| StorageBackendError::backend("stored foreign-key identities", error))?;
    let scalars = PersistentTableScalars {
        catalog,
        name: &name,
        object_id,
        columns: &columns,
        documents: &*documents,
        limits,
        control,
    }
    .read_from(&mut reader.input())
    .await?;
    let contents = TableContents {
        object_id,
        storage_generation,
        security: TableMetadata::from_admitted(security, control.cancellation())
            .map_err(sql_error)?,
        documents,
        text,
        vectors,
        values: uqa_execution::catalog::index::value::ValueIndexes::default(),
        fts_fields: TableMetadata::from_admitted(fts_fields, control.cancellation())
            .map_err(sql_error)?,
        columns,
        constraints: constraints.into_native().map_err(sql_error)?,
        analyzer,
        next_id: scalars.next_id,
        maps_integer_keys: scalars.maps_integer_keys,
        statistics: scalars.statistics,
        statistics_loaded: true,
        statistics_dirty: scalars.statistics_dirty,
        maintenance: StatisticsMaintenance::default(),
    };
    Ok((relation, Arc::new(contents.into_state(Some(control))?)))
}

async fn read_vectors(
    input: &mut ImageDecoder<uqa_storage::vector_index::VectorIndexes, StorageBackendError>,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<uqa_storage::vector_index::VectorIndexes> {
    loop {
        reader.checkpoint().await?;
        if let Some(value) = input.advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        )? {
            return Ok(value);
        }
    }
}

struct PersistentTableScalars<'a> {
    catalog: &'a dyn CatalogFacade,
    name: &'a str,
    object_id: [u8; 16],
    columns: &'a [uqa_sql::ast::ColumnDef],
    documents: &'a dyn uqa_storage::DocumentStore,
    limits: CatalogInputLimits,
    control: &'a StorageReadControl,
}

struct ScalarState {
    statistics: uqa_planner::ColumnStatistics,
    statistics_dirty: bool,
    next_id: u128,
    maps_integer_keys: bool,
}

impl PersistentTableScalars<'_> {
    async fn read_from(self, input: &mut AsyncImageInput<'_>) -> StorageBackendResult<ScalarState> {
        let Self {
            catalog,
            name,
            object_id,
            columns,
            documents,
            limits,
            control,
        } = self;
        let statistics = statistics(catalog, name, limits, control, input).await?;
        let statistics_dirty = (statistics.is_empty() && !columns.is_empty())
            || StatisticsMaintenance::load_for_from(
                catalog, name, object_id, limits, control, input,
            )
            .await?
            .invalidates_existing_statistics();
        let mut maximum = documents.begin_max_doc_id_input(limits.maximum_key_bytes, control)?;
        let max_id = loop {
            input.checkpoint().await?;
            if let MaximumDocIdStep::Complete(value) = maximum.advance()? {
                break value;
            }
        };
        drop(maximum);
        let mut legacy = false;
        for column in columns {
            input.checkpoint().await?;
            if column
                .auto_increment
                .as_ref()
                .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy)
            {
                legacy = true;
                break;
            }
        }
        let persisted_next_id = if legacy {
            identifiers::load_legacy_document_id_watermark_from(
                catalog, name, limits, control, input,
            )
            .await?
        } else {
            None
        };
        let maps_integer_keys =
            if uqa_sql::semantics::key_identity::single_integer_primary_key_from(
                columns,
                control.cancellation(),
                input,
            )
            .await
            .map_err(sql_error)?
            .is_some()
            {
                identifiers::key_mapping::load_from(catalog, object_id, limits, control, input)
                    .await?
            } else {
                true
            };
        Ok(ScalarState {
            statistics,
            statistics_dirty,
            next_id: identifiers::restored_document_id_watermark(max_id, persisted_next_id),
            maps_integer_keys,
        })
    }
}

async fn statistics(
    catalog: &dyn CatalogFacade,
    name: &str,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<uqa_planner::ColumnStatistics> {
    use uqa_planner::stored_column_statistics::{StoredColumnStatistics, StoredColumnStats};
    let mut source = catalog.begin_column_stats_input(name, limits, control)?;
    let mut statistics =
        StoredColumnStatistics::new(control.memory(), control.cancellation(), input.cancellation);
    loop {
        input.checkpoint().await?;
        match source.advance()? {
            CatalogInputStep::Pending => (),
            CatalogInputStep::Row(row) => {
                statistics
                    .insert(StoredColumnStats {
                        column_name: &row.column_name,
                        distinct_count: row.distinct_count,
                        null_count: row.null_count,
                        min_value: row.min_value.as_deref(),
                        max_value: row.max_value.as_deref(),
                        row_count: row.row_count,
                        histogram_json: &row.histogram_json,
                        mcv_values_json: &row.mcv_values_json,
                        mcv_frequencies_json: &row.mcv_frequencies_json,
                    })
                    .map_err(|error| {
                        StorageBackendError::backend("stored column statistics", error)
                    })?;
            }
            CatalogInputStep::Complete => break,
        }
    }
    statistics
        .finish()
        .map_err(|error| StorageBackendError::backend("stored column statistics", error))
}

fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    match error {
        uqa_sql::SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("stored table declaration", error),
    }
}

#[cfg(test)]
mod tests;
