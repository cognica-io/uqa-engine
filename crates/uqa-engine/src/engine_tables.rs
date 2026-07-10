//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    standard_analyzer, Analyzer, AnalyzerPhase, Arc, AtomicBool, BTreeMap, DocId, Document,
    DocumentStore, Engine, FieldName, IVFIndex, IVFIndexParams, InvertedIndex, MemoryDocumentStore,
    MemoryInvertedIndex, MemoryVectorIndex, PersistentVectorIndexParams, RwLock,
    StorageBackendError, StorageBackendResult, TableSchema, TableState, VectorFieldSchema,
    VectorIndex,
};

impl Engine {
    pub(crate) fn is_persistent(&self) -> bool {
        self.catalog.is_some()
    }

    pub(crate) fn save_table_schema(&self, name: &str, table: &TableState) {
        let _ = self.try_save_table_schema(name, table);
    }

    pub(crate) fn try_save_table_schema(
        &self,
        name: &str,
        table: &TableState,
    ) -> StorageBackendResult<()> {
        let Some(catalog) = self.catalog.as_ref() else {
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
        let columns_json =
            serde_json::to_string(&*table.columns.read()).map_err(StorageBackendError::from)?;
        catalog.save_table(&TableSchema {
            name: name.to_string(),
            analyzer_json,
            fts_fields: table.fts_fields(),
            vector_fields,
            columns_json,
        })
    }

    pub(crate) fn try_persist_table_schema(&self, table: &str) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(false);
        };
        let Some(t) = self.table(table) else {
            return Ok(false);
        };
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
    ) {
        let name = self.relation_name_for_create(&name.into());
        let (docs, inv): (Box<dyn DocumentStore>, Box<dyn InvertedIndex>) =
            if let Some(backend) = self.backend.as_ref() {
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
            value_indexes: RwLock::new(BTreeMap::new()),
            doc_count_cache: std::sync::atomic::AtomicU64::new(0),
            doc_count_dirty: AtomicBool::new(true),
        };
        let table_arc = Arc::new(table);
        self.tables.write().insert(name.clone(), table_arc.clone());
        if self.is_persistent() {
            self.save_table_schema(&name, &table_arc);
        }
    }

    /// Register a vector field on a table. Existing document values in the
    /// same field are indexed immediately; later calls to [`Engine::add_vector`]
    /// or [`Engine::add_document_with_vectors`] keep it current. `CREATE INDEX
    /// ... USING ivf` upgrades the field to an IVF backend.
    pub fn create_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
    ) -> bool {
        self.install_vector_field(table, field.into(), dimensions, None, false, true)
    }

    pub(crate) fn rebuild_ivf_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> bool {
        self.install_vector_field(table, field.into(), dimensions, Some(params), true, true)
    }

    pub(crate) fn rebuild_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
    ) -> bool {
        self.install_vector_field(table, field.into(), dimensions, None, true, true)
    }

    pub(crate) fn drop_ivf_vector_field_index(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
    ) -> bool {
        self.install_vector_field(table, field.into(), dimensions, None, true, true)
    }

    pub(crate) fn drop_vector_index_metadata(
        &self,
        table: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        if let Some(backend) = self.backend.as_ref() {
            backend.drop_vector_index_metadata(table, field)?;
        }
        Ok(())
    }

    pub(crate) fn restore_ivf_vector_field(
        &self,
        table: &str,
        field: impl Into<FieldName>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let field = field.into();
        let idx = self.build_vector_index_for_restore(table, &field, dimensions, params);
        t.vector_indexes.write().insert(field, idx);
        true
    }

    pub(crate) fn restore_fts_field_from_catalog(
        &self,
        table: &str,
        field: &str,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        let t = self
            .table(table)
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        if let Some(analyzer_name) = analyzer {
            let analyzer = self.resolve_analyzer(analyzer_name)?;
            t.inverted_index
                .write()
                .set_field_analyzer(field, analyzer, AnalyzerPhase::Both)
                .map_err(|e| format!("restore_fts_field: {e}"))?;
            self.table_field_analyzers.write().insert(
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
        params: Option<IVFIndexParams>,
        replace_existing: bool,
        persist_schema: bool,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        if !replace_existing {
            if let Some(existing) = t.vector_indexes.read().get(&field) {
                return existing.dimensions() == dimensions;
            }
        }
        let mut idx = self.build_vector_index(table, &field, dimensions, params);
        if idx.count() == 0 {
            Self::backfill_vector_index(&t, &field, idx.as_mut());
        }
        t.vector_indexes.write().insert(field, idx);
        if persist_schema && self.is_persistent() {
            self.save_table_schema(table, &t);
        }
        true
    }

    fn build_vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        params: Option<IVFIndexParams>,
    ) -> Box<dyn VectorIndex> {
        self.build_vector_index_with_initialize(table, field, dimensions, params, true)
    }

    pub(crate) fn build_vector_index_for_restore(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> Box<dyn VectorIndex> {
        self.build_vector_index_with_initialize(table, field, dimensions, Some(params), false)
    }

    pub(crate) fn build_vector_index_with_initialize(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        params: Option<IVFIndexParams>,
        initialize: bool,
    ) -> Box<dyn VectorIndex> {
        if let Some(backend) = self.backend.as_ref() {
            backend.vector_index(
                table,
                field,
                dimensions,
                params.map(|params| PersistentVectorIndexParams {
                    nlist: params.nlist,
                    nprobe: params.nprobe,
                    train_threshold: params.train_threshold,
                    initialize,
                }),
            )
        } else if let Some(params) = params {
            Box::new(IVFIndex::with_params(
                dimensions,
                params.nlist,
                params.nprobe,
                params.train_threshold,
            ))
        } else {
            Box::new(MemoryVectorIndex::new(dimensions))
        }
    }

    fn backfill_vector_index(table: &TableState, field: &str, idx: &mut dyn VectorIndex) {
        let docs = table.document_store.read().snapshot();
        for (doc_id, document) in docs.iter_all() {
            let Some(value) = document.get(field) else {
                continue;
            };
            if let Some(vectors) = Self::field_index_vectors(table, field, value) {
                idx.add_many(doc_id, vectors);
            }
        }
    }

    pub fn add_vector(&self, table: &str, doc_id: DocId, field: &str, vector: Vec<f32>) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut idxs = t.vector_indexes.write();
        let Some(idx) = idxs.get_mut(field) else {
            return false;
        };
        idx.as_mut().add(doc_id, vector);
        true
    }

    pub fn add_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        field: &str,
        vectors: Vec<Vec<f32>>,
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut idxs = t.vector_indexes.write();
        let Some(idx) = idxs.get_mut(field) else {
            return false;
        };
        idx.as_mut().add_many(doc_id, vectors);
        true
    }

    pub fn add_document_with_vectors(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<f32>>,
    ) {
        let vector_values = vectors
            .into_iter()
            .map(|(field, vector)| (field, vec![vector]))
            .collect();
        self.add_document_with_vector_values(table, doc_id, document, vector_values);
    }

    pub fn add_document_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: BTreeMap<FieldName, Vec<Vec<f32>>>,
    ) {
        self.add_document(table, doc_id, document);
        for (field, vectors) in vectors {
            self.add_vector_values(table, doc_id, &field, vectors);
        }
    }

    pub fn create_default_table(&self, name: impl Into<String>, fts_fields: Vec<FieldName>) {
        self.create_table(name, standard_analyzer("english"), fts_fields);
    }
}
