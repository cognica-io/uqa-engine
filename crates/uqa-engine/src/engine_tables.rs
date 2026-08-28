//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    standard_analyzer, Analyzer, AnalyzerPhase, Arc, AtomicBool, BTreeMap, DocId, Document,
    DocumentStore, Engine, FieldName, HNSWIndex, IVFIndex, InvertedIndex, MemoryDocumentStore,
    MemoryInvertedIndex, MemoryVectorIndex, RelationIdentity, RwLock, SQLError,
    StorageBackendError, StorageBackendResult, TableSchema, TableState, VectorFieldSchema,
    VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};

impl Engine {
    pub(crate) fn is_persistent(&self) -> bool {
        self.storage.catalog.is_some()
    }

    pub(crate) fn try_save_table_schema(
        &self,
        name: &str,
        table: &TableState,
    ) -> StorageBackendResult<()> {
        let columns = table.columns.read().clone();
        self.try_save_table_schema_with_columns(name, table, &columns)
    }

    pub(crate) fn try_save_table_schema_with_columns(
        &self,
        name: &str,
        table: &TableState,
        columns: &[uqa_sql::ast::ColumnDef],
    ) -> StorageBackendResult<()> {
        let constraints = uqa_sql::ast::TableConstraintSet {
            checks: table.table_checks.read().clone(),
            foreign_keys: table.foreign_keys.read().clone(),
            key_constraints: table.key_constraints.read().clone(),
            persistence: table.persistence,
            on_commit: table.on_commit,
            hierarchy: table.hierarchy.read().clone(),
        };
        self.try_save_table_schema_with_components(name, table, columns, &constraints)
    }

    pub(crate) fn try_save_table_schema_with_components(
        &self,
        name: &str,
        table: &TableState,
        columns: &[uqa_sql::ast::ColumnDef],
        constraints: &uqa_sql::ast::TableConstraintSet,
    ) -> StorageBackendResult<()> {
        if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            return Ok(());
        }
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let analyzer_json =
            serde_json::to_string(&*table.analyzer.read()).map_err(StorageBackendError::from)?;
        let vector_fields: Vec<VectorFieldSchema> = table
            .vector_indexes
            .read()
            .iter()
            .map(|(field, idx)| VectorFieldSchema {
                field: field.clone(),
                dimensions: idx.dimensions(),
            })
            .collect();
        let columns_json = serde_json::to_string(columns).map_err(StorageBackendError::from)?;
        let constraints_json =
            serde_json::to_string(constraints).map_err(StorageBackendError::from)?;
        catalog.save_table(&TableSchema {
            relation: RelationIdentity::from_legacy_name(name)
                .map_err(StorageBackendError::Other)?,
            analyzer_json,
            fts_fields: table.fts_fields(),
            vector_fields,
            columns_json,
            constraints_json,
        })?;
        self.note_table_catalog_changed();
        Ok(())
    }

    pub(crate) fn try_persist_table_schema(&self, table: &str) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        let t = self.try_table(&table_name)?.ok_or_else(|| {
            StorageBackendError::Other(format!("table `{table_name}` does not exist"))
        })?;
        self.try_save_table_schema(&table_name, &t)?;
        Ok(true)
    }

    /// Register a table. `fts_fields` is the list of field names that are
    /// tokenized into the inverted index when documents are inserted.
    /// Other fields are still stored in the document store but are not
    /// queryable via `text_match` / [`uqa_operators::TermOperator`].
    pub fn create_table(
        &self,
        name: impl Into<String>,
        analyzer: Analyzer,
        fts_fields: Vec<FieldName>,
    ) -> StorageBackendResult<()> {
        let raw_name = name.into();
        self.with_implicit_storage_transaction(move |engine| {
            engine.create_table_inner(
                &raw_name,
                analyzer,
                fts_fields,
                uqa_sql::ast::RelationPersistence::Permanent,
                uqa_sql::ast::OnCommitAction::PreserveRows,
            )
        })
    }

    pub(crate) fn create_table_with_lifecycle(
        &self,
        name: &str,
        analyzer: Analyzer,
        fts_fields: Vec<FieldName>,
        persistence: uqa_sql::ast::RelationPersistence,
        on_commit: uqa_sql::ast::OnCommitAction,
    ) -> StorageBackendResult<()> {
        if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            return self.create_table_inner(name, analyzer, fts_fields, persistence, on_commit);
        }
        let name = name.to_string();
        self.with_implicit_storage_transaction(move |engine| {
            engine.create_table_inner(&name, analyzer, fts_fields, persistence, on_commit)
        })
    }

    fn create_table_inner(
        &self,
        raw_name: &str,
        analyzer: Analyzer,
        fts_fields: Vec<FieldName>,
        persistence: uqa_sql::ast::RelationPersistence,
        on_commit: uqa_sql::ast::OnCommitAction,
    ) -> StorageBackendResult<()> {
        let name = if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            self.try_temporary_relation_name_for_create(raw_name)
                .map_err(StorageBackendError::Other)?
        } else {
            self.try_relation_name_for_create(raw_name)
                .map_err(StorageBackendError::Other)?
        };
        let relation = Self::resolved_relation_identity(&name)?;
        if let Some(kind) = self.relation_kind_at(&name)? {
            return Err(StorageBackendError::Other(format!(
                "relation `{name}` already exists as {kind}"
            )));
        }
        let (docs, inv): (Box<dyn DocumentStore>, Box<dyn InvertedIndex>) =
            if persistence == uqa_sql::ast::RelationPersistence::Temporary {
                (
                    Box::new(MemoryDocumentStore::new()),
                    Box::new(MemoryInvertedIndex::new(analyzer.clone())),
                )
            } else if let Some(backend) = self.storage.backend.as_ref() {
                (
                    backend.document_store(&name),
                    backend.inverted_index(&name, analyzer.clone()),
                )
            } else {
                (
                    Box::new(MemoryDocumentStore::new()),
                    Box::new(MemoryInvertedIndex::new(analyzer.clone())),
                )
            };
        let table = TableState {
            document_store: RwLock::new(docs),
            inverted_index: RwLock::new(inv),
            vector_indexes: RwLock::new(BTreeMap::new()),
            fts_fields: RwLock::new(fts_fields),
            columns: RwLock::new(Vec::new()),
            next_id: parking_lot::Mutex::new(1),
            analyzer: RwLock::new(analyzer),
            column_stats: RwLock::new(BTreeMap::new()),
            column_stats_loaded: AtomicBool::new(true),
            column_stats_dirty: AtomicBool::new(true),
            table_checks: RwLock::new(Vec::new()),
            foreign_keys: RwLock::new(Vec::new()),
            key_constraints: RwLock::new(Vec::new()),
            hierarchy: RwLock::new(uqa_sql::ast::TableHierarchy::default()),
            value_indexes: RwLock::new(BTreeMap::new()),
            doc_count_cache: std::sync::atomic::AtomicU64::new(0),
            doc_count_dirty: AtomicBool::new(true),
            persistence,
            on_commit,
        };
        let table_arc = Arc::new(table);
        if self.is_persistent() && persistence != uqa_sql::ast::RelationPersistence::Temporary {
            self.try_save_table_schema(&name, &table_arc)?;
        }
        self.storage.tables.write().insert(relation, table_arc);
        self.clear_regtype_output_cache();
        if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            self.note_table_catalog_changed();
        }
        Ok(())
    }

    /// Register a vector field on a table. Existing document values in the
    /// same field are indexed immediately; later calls to [`Engine::add_vector`]
    /// or [`Engine::add_document_with_vectors`] keep it current. `CREATE INDEX
    /// ... USING ivf` or `CREATE INDEX ... USING hnsw` upgrades the field to
    /// the corresponding approximate backend.
    pub fn create_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        let field = field.into();
        self.with_implicit_storage_transaction(|engine| {
            engine.install_vector_field(
                table,
                field,
                dimensions,
                VectorIndexSpec::BruteForce,
                false,
                true,
            )
        })
    }

    pub(crate) fn rebuild_vector_field_with_spec(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
        spec: VectorIndexSpec,
    ) -> StorageBackendResult<bool> {
        let field = field.into();
        self.with_implicit_storage_transaction(|engine| {
            engine.install_vector_field(table, field, dimensions, spec, true, true)
        })
    }

    pub(crate) fn rebuild_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        self.rebuild_vector_field_with_spec(table, field, dimensions, VectorIndexSpec::BruteForce)
    }

    pub(crate) fn drop_vector_field_index(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        self.rebuild_vector_field(table, field, dimensions)
    }

    pub(crate) fn drop_vector_index_metadata(
        &self,
        table: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        if let Some(backend) = self.storage.backend.as_ref() {
            backend.drop_vector_index_metadata(table, field)?;
        }
        Ok(())
    }

    pub(crate) fn restore_vector_field_index(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
        spec: VectorIndexSpec,
    ) -> StorageBackendResult<bool> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        let field = field.into();
        let idx = self.build_vector_index_for_restore(table, &field, dimensions, spec)?;
        t.vector_indexes.write().insert(field, idx);
        Ok(true)
    }

    pub(crate) fn restore_fts_field_from_catalog(
        &self,
        table: &str,
        field: &str,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        let t = self
            .try_table(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        if let Some(analyzer_name) = analyzer {
            let analyzer = self.resolve_analyzer(analyzer_name)?;
            t.inverted_index
                .write()
                .set_field_analyzer(field, analyzer, AnalyzerPhase::Both)
                .map_err(|e| format!("restore_fts_field: {e}"))?;
            self.durable.table_field_analyzers.write().insert(
                (table.to_string(), field.to_string()),
                (analyzer_name.to_string(), "both".to_string()),
            );
        }
        {
            let mut fts = t.fts_fields.write();
            if !fts.iter().any(|f| f == field) {
                fts.push(field.to_string());
            }
        }
        Ok(())
    }

    fn install_vector_field(
        &self,
        table: &str,
        field: FieldName,
        dimensions: u32,
        spec: VectorIndexSpec,
        replace_existing: bool,
        persist_schema: bool,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.try_resolve_table_name(table)? else {
            return Err(StorageBackendError::Other(format!(
                "table `{table}` does not exist"
            )));
        };
        let Some(t) = self.try_table(&table_name)? else {
            return Err(StorageBackendError::Other(format!(
                "table `{table_name}` disappeared while installing vector field `{field}`"
            )));
        };
        if !replace_existing {
            if let Some(existing) = t.vector_indexes.read().get(&field) {
                let existing_dimensions = existing.dimensions();
                if existing_dimensions == dimensions {
                    return Ok(false);
                }
                return Err(StorageBackendError::Other(format!(
                    "vector field `{table_name}.{field}` has dimension {existing_dimensions}, requested {dimensions}"
                )));
            }
        }
        let mut idx = self.build_vector_index(&table_name, &field, dimensions, spec)?;
        if idx.count()? == 0 {
            Self::backfill_vector_index(&t, &field, idx.as_mut())?;
        }
        idx.initialize()?;
        let old = t.vector_indexes.write().insert(field.clone(), idx);
        if persist_schema && self.is_persistent() {
            if let Err(err) = self.try_save_table_schema(&table_name, &t) {
                let mut indexes = t.vector_indexes.write();
                indexes.remove(&field);
                if let Some(old) = old {
                    indexes.insert(field, old);
                }
                return Err(err);
            }
        }
        Ok(true)
    }

    fn build_vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.build_vector_index_with_mode(
            table,
            field,
            dimensions,
            spec,
            VectorIndexOpenMode::Create,
        )
    }

    pub(crate) fn build_vector_index_for_restore(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.build_vector_index_with_mode(
            table,
            field,
            dimensions,
            spec,
            VectorIndexOpenMode::Restore,
        )
    }

    pub(crate) fn build_vector_index_with_mode(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
        mode: VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        // Callers pass the canonical name after resolving the table. Looking
        // it up in the already-loaded registry is essential during catalog
        // restoration: a normal `try_table` lookup would recursively enter
        // catalog synchronization while its mutex is already held.
        let relation =
            RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
        let temporary = self
            .storage
            .tables
            .read()
            .get(&relation)
            .is_some_and(|table| table.persistence == uqa_sql::ast::RelationPersistence::Temporary);
        if temporary {
            Self::memory_vector_index(dimensions, spec)
        } else if let Some(backend) = self.storage.backend.as_ref() {
            backend.vector_index(table, field, dimensions, spec, mode)
        } else {
            Self::memory_vector_index(dimensions, spec)
        }
    }

    fn memory_vector_index(
        dimensions: u32,
        spec: VectorIndexSpec,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        let index: Box<dyn VectorIndex> = match spec {
            VectorIndexSpec::BruteForce => Box::new(MemoryVectorIndex::new(dimensions)),
            VectorIndexSpec::IVF(params) => Box::new(IVFIndex::with_params(
                dimensions,
                params.nlist,
                params.nprobe,
                params.train_threshold,
            )),
            VectorIndexSpec::HNSW(params) => Box::new(HNSWIndex::with_params(dimensions, params)?),
        };
        Ok(index)
    }

    fn backfill_vector_index(
        table: &TableState,
        field: &str,
        idx: &mut dyn VectorIndex,
    ) -> StorageBackendResult<()> {
        let docs = table.document_store.read().snapshot()?;
        for (doc_id, document) in docs.iter_all()? {
            let Some(value) = document.get(field) else {
                continue;
            };
            if let Some(vectors) = Self::field_index_vectors(table, field, value)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?
            {
                idx.add_many(doc_id, vectors)?;
            }
        }
        Ok(())
    }

    pub fn add_vector(
        &self,
        table: &str,
        doc_id: DocId,
        field: &str,
        vector: Vec<f32>,
    ) -> Result<bool, SQLError> {
        self.with_implicit_row_write_transaction(
            table,
            doc_id,
            uqa_sql::ast::LockStrength::ForNoKeyUpdate,
            |engine| engine.add_vector_inner(table, doc_id, field, vector),
        )
    }

    fn add_vector_inner(
        &self,
        table: &str,
        doc_id: DocId,
        field: &str,
        vector: Vec<f32>,
    ) -> Result<bool, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let mut idxs = t.vector_indexes.write();
        let Some(idx) = idxs.get_mut(field) else {
            return Err(SQLError::TypeMismatch(format!(
                "vector field `{table}.{field}` is not registered"
            )));
        };
        idx.as_mut()
            .add(doc_id, vector)
            .map_err(|error| SQLError::Internal(format!("index document vector: {error}")))?;
        drop(idxs);
        self.note_table_data_changed();
        self.note_row_changed(table, doc_id)?;
        Ok(true)
    }

    pub fn add_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        field: &str,
        vectors: Vec<Vec<f32>>,
    ) -> Result<bool, SQLError> {
        self.with_implicit_row_write_transaction(
            table,
            doc_id,
            uqa_sql::ast::LockStrength::ForNoKeyUpdate,
            |engine| engine.add_vector_values_inner(table, doc_id, field, vectors),
        )
    }

    fn add_vector_values_inner(
        &self,
        table: &str,
        doc_id: DocId,
        field: &str,
        vectors: Vec<Vec<f32>>,
    ) -> Result<bool, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let mut idxs = t.vector_indexes.write();
        let Some(idx) = idxs.get_mut(field) else {
            return Err(SQLError::TypeMismatch(format!(
                "vector field `{table}.{field}` is not registered"
            )));
        };
        idx.as_mut()
            .add_many(doc_id, vectors)
            .map_err(|error| SQLError::Internal(format!("index document vectors: {error}")))?;
        drop(idxs);
        self.note_table_data_changed();
        self.note_row_changed(table, doc_id)?;
        Ok(true)
    }

    pub(crate) fn validate_vector_values(
        &self,
        table: &str,
        vectors: &BTreeMap<FieldName, Vec<Vec<f32>>>,
    ) -> Result<(), SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let indexes = t.vector_indexes.read();
        for (field, values) in vectors {
            let Some(index) = indexes.get(field) else {
                return Err(SQLError::UnknownColumn(format!("{table}.{field}")));
            };
            let expected = usize::try_from(index.dimensions()).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "vector field `{table}.{field}` dimension exceeds the platform usize range"
                ))
            })?;
            if let Some(vector) = values.iter().find(|vector| vector.len() != expected) {
                return Err(SQLError::VectorDimMismatch {
                    expected,
                    actual: vector.len(),
                });
            }
            if let Some((ordinal, component, value)) =
                values.iter().enumerate().find_map(|(ordinal, vector)| {
                    vector
                        .iter()
                        .copied()
                        .enumerate()
                        .find(|(_, value)| !value.is_finite())
                        .map(|(component, value)| (ordinal, component, value))
                })
            {
                return Err(SQLError::TypeMismatch(format!(
                    "vector field `{table}.{field}` vector {ordinal} component {component} must be finite, got {value}"
                )));
            }
        }
        Ok(())
    }

    pub fn add_document_with_vectors(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<f32>>,
    ) -> Result<(), SQLError> {
        let vector_values = vectors
            .into_iter()
            .map(|(field, vector)| (field, vec![vector]))
            .collect();
        self.add_document_with_vector_values(table, doc_id, document, vector_values)
    }

    pub fn add_document_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<Vec<f32>>>,
    ) -> Result<(), SQLError> {
        self.with_implicit_row_write_transaction(
            table,
            doc_id,
            uqa_sql::ast::LockStrength::ForUpdate,
            |engine| {
                engine
                    .add_document_with_vector_values_inner(table, doc_id, document, vectors, false)
            },
        )
    }

    pub(crate) fn add_document_with_vector_values_inner(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<Vec<f32>>>,
        known_new: bool,
    ) -> Result<(), SQLError> {
        self.validate_vector_values(table, &vectors)?;
        self.add_document_impl(table, doc_id, document, known_new)?;
        for (field, vectors) in vectors {
            self.add_vector_values_inner(table, doc_id, &field, vectors)?;
        }
        Ok(())
    }

    pub(crate) fn add_prepared_document_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<Vec<f32>>>,
        known_new: bool,
    ) -> Result<(), SQLError> {
        self.with_implicit_row_write_transaction(
            table,
            doc_id,
            uqa_sql::ast::LockStrength::ForUpdate,
            |engine| {
                engine.add_prepared_document_with_vector_values_inner(
                    table, doc_id, document, vectors, known_new,
                )
            },
        )
    }

    pub(crate) fn add_prepared_document_with_vector_values_inner(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<Vec<f32>>>,
        known_new: bool,
    ) -> Result<(), SQLError> {
        self.validate_vector_values(table, &vectors)?;
        self.add_prepared_document_impl(table, doc_id, document, known_new)?;
        for (field, vectors) in vectors {
            self.add_vector_values_inner(table, doc_id, &field, vectors)?;
        }
        Ok(())
    }

    pub fn create_default_table(
        &self,
        name: impl Into<String>,
        fts_fields: Vec<FieldName>,
    ) -> StorageBackendResult<()> {
        self.create_table(name, standard_analyzer("english"), fts_fields)
    }
}
