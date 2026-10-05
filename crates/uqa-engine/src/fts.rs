//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    analyzer_registry, Arc, BTreeMap, DocId, Document, Engine, FieldName, FtsIndexStat, SQLError,
    TableState, Value,
};
use uqa_storage::inverted_index::DocumentTextSource;
use uqa_storage::{InvertedIndex, StorageBackendResult};

impl Engine {
    pub(crate) fn fts_fields_for_table(&self, name: &str) -> Result<Vec<FieldName>, SQLError> {
        Ok(self
            .try_query_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve table `{name}`: {err}")))?
            .map_or_else(Vec::new, |table| table.fts_fields()))
    }

    /// Validate the physical text-search contract for one concrete field.
    /// A declared TEXT column is not searchable until it has been registered
    /// in a GIN/FTS index; treating that state as an empty posting list hides a
    /// schema/configuration error from both the public search API and the
    /// operator-tree executor.
    pub(crate) fn validate_text_search_field(
        &self,
        table: &str,
        field: &str,
    ) -> Result<(), SQLError> {
        let Some(table_state) = self
            .try_query_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve text-search table: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        uqa_sql::semantics::text_indexes::require_physical_text_index(
            table,
            field,
            &table_state.fts_fields(),
            || table_state.columns.read().clone(),
        )
    }

    pub fn fts_index_stats(
        &self,
        table_filter: Option<&str>,
    ) -> Result<Vec<FtsIndexStat>, SQLError> {
        self.with_direct_read_snapshot(|engine| {
            engine.fts_index_stats_with_tables(table_filter, |name| {
                engine
                    .bind_query_table_read(name)
                    .map(|binding| binding.value)
            })
        })
    }

    pub(crate) fn fts_index_stats_in_execution(
        &self,
        table_filter: Option<&str>,
    ) -> Result<Vec<FtsIndexStat>, SQLError> {
        self.fts_index_stats_with_tables(table_filter, |name| self.require_query_table(name))
    }

    fn fts_index_stats_with_tables(
        &self,
        table_filter: Option<&str>,
        bind: impl Fn(&str) -> Result<Arc<TableState>, SQLError>,
    ) -> Result<Vec<FtsIndexStat>, SQLError> {
        let mut out = Vec::new();
        for table_name in self.fts_stats_table_names(table_filter)? {
            let table = bind(&table_name)?;
            let mut fields = table.fts_fields();
            fields.sort();
            let index = table.inverted_index.read();
            let index = uqa_execution::serializable::text::ObservedTextIndex::new(
                index.as_ref(),
                self.serializable_table_state_read(&table)?,
                table.columns.snapshot(),
            );
            for field in fields {
                let analyzer = self
                    .table_field_analyzer_in_execution(&table_name, &field)
                    .map_err(SQLError::Internal)?
                    .map_or_else(
                        || analyzer_registry::DEFAULT_ANALYZER_NAME.to_string(),
                        |(name, _)| name,
                    );
                let doc_length_count = index.doc_length_count(Some(&field)).map_err(|error| {
                    uqa_execution::storage_errors::storage_error(
                        "read FTS document-length count",
                        &error,
                    )
                })?;
                out.push(FtsIndexStat {
                    table_name: table_name.clone(),
                    field: field.clone(),
                    analyzer,
                    posting_count: index.posting_count(Some(&field)).map_err(|error| {
                        uqa_execution::storage_errors::storage_error(
                            "read FTS posting count",
                            &error,
                        )
                    })?,
                    doc_length_count,
                    indexed_doc_count: doc_length_count,
                    term_count: index.term_count(Some(&field)).map_err(|error| {
                        uqa_execution::storage_errors::storage_error("read FTS term count", &error)
                    })?,
                    total_field_length: index.total_field_length(&field).map_err(|error| {
                        uqa_execution::storage_errors::storage_error(
                            "read FTS field length",
                            &error,
                        )
                    })?,
                });
            }
        }
        Ok(out)
    }

    fn fts_stats_table_names(&self, table_filter: Option<&str>) -> Result<Vec<String>, SQLError> {
        self.synchronize_table_catalog()
            .map_err(|err| SQLError::Internal(format!("refresh table catalog: {err}")))?;
        let mut names = if let Some(name) = table_filter {
            vec![self
                .try_resolve_query_table_name(name)
                .map_err(|err| SQLError::Internal(format!("resolve table filter: {err}")))?
                .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?]
        } else if let Some(tables) = self.query_table_snapshots.as_ref() {
            tables
                .keys()
                .map(uqa_core::RelationIdentity::qualified_name)
                .collect()
        } else {
            self.storage
                .tables
                .read()
                .keys()
                .map(uqa_core::RelationIdentity::qualified_name)
                .collect()
        };
        names.sort_unstable();
        Ok(names)
    }

    /// The text of the table's indexed fields, which a rebuild reads a page at a time from a snapshot of its documents instead of collecting it first.
    pub(crate) fn fts_source(
        t: &Arc<TableState>,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> StorageBackendResult<DocumentTextSource> {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        let documents = t.document_store.read().snapshot()?;
        Ok(DocumentTextSource::new(
            documents,
            t.fts_fields(),
            cancellation.cloned(),
        ))
    }

    pub(crate) fn rebuild_fts_index(t: &Arc<TableState>) -> StorageBackendResult<()> {
        let mut source = Self::fts_source(t, None)?;
        t.inverted_index.write().try_rebuild_documents(&mut source)
    }

    pub(crate) fn rebuild_fts_index_cancellable(
        t: &Arc<TableState>,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        let mut source = Self::fts_source(t, Some(cancellation))?;
        t.inverted_index
            .write()
            .try_rebuild_documents_cancellable(&mut source, cancellation)
    }

    pub fn add_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        self.with_implicit_row_write_transaction(
            table,
            doc_id,
            uqa_sql::ast::LockStrength::ForUpdate,
            |engine| {
                engine.add_document_impl(
                    table,
                    doc_id,
                    document,
                    uqa_execution::mutation::publication::InsertedIdentity::Unknown,
                )
            },
        )
    }

    pub(crate) fn add_document_impl(
        &self,
        table: &str,
        doc_id: DocId,
        mut document: Document,
        inserted: uqa_execution::mutation::publication::InsertedIdentity,
    ) -> Result<(), SQLError> {
        uqa_execution::mutation::assignment::refresh_stored_generated_columns(
            self.mutation_assignment_context(),
            table,
            &mut document,
        )?;
        uqa_execution::serializable::observe_row_write(self, table, doc_id)?;
        self.add_prepared_document_impl(table, doc_id, document, inserted)
    }

    pub(crate) fn add_prepared_document_impl(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        inserted: uqa_execution::mutation::publication::InsertedIdentity,
    ) -> Result<(), SQLError> {
        self.add_prepared_document_impl_with_fts(table, doc_id, document, inserted, true, None)
    }

    pub(crate) fn add_prepared_document_without_fts_impl(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        inserted: uqa_execution::mutation::publication::InsertedIdentity,
    ) -> Result<(), SQLError> {
        self.add_prepared_document_impl_with_fts(table, doc_id, document, inserted, false, None)
    }

    pub(crate) fn add_prepared_stored_document_impl(
        &self,
        table: &str,
        doc_id: DocId,
        document: uqa_storage::StoredDocument,
        inserted: uqa_execution::mutation::publication::InsertedIdentity,
    ) -> Result<(), SQLError> {
        let (fields, metadata) = document.into_parts();
        self.add_prepared_document_impl_with_fts(
            table,
            doc_id,
            fields,
            inserted,
            true,
            Some(metadata),
        )
    }

    pub(crate) fn prepared_document_text_fields(
        &self,
        table: &str,
        document: &Document,
    ) -> Result<BTreeMap<FieldName, String>, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let mut text_fields = BTreeMap::new();
        for name in &t.fts_fields() {
            if let Some(Value::Str(value)) = document.get(name) {
                text_fields.insert(name.clone(), value.clone());
            }
        }
        Ok(text_fields)
    }

    pub(crate) fn add_prepared_fts_documents(
        &self,
        table: &str,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> Result<(), SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let result = uqa_execution::serializable::text::add_documents(
            self,
            table,
            t.columns.snapshot(),
            t.inverted_index.write().as_mut(),
            documents,
        );
        result
    }

    /// Replacement is one atomic inverted-index operation even when the new document has no indexed text. Skipping an empty field map would leave stale postings from the previous version; remove-then-add would expose a destructive failure window when analysis fails. Only a document known to be new has no previous version, and without indexed text it leaves the index alone.
    fn publish_prepared_document_text(
        &self,
        table_name: &str,
        table: &TableState,
        doc_id: DocId,
        text_fields: BTreeMap<FieldName, String>,
        known_new: bool,
    ) -> Result<(), SQLError> {
        if !uqa_execution::mutation::publication::document_changes_text_index(
            known_new,
            &text_fields,
        ) {
            return Ok(());
        }
        uqa_execution::serializable::text::add_document(
            self,
            table_name,
            table.columns.snapshot(),
            table.inverted_index.write().as_mut(),
            doc_id,
            text_fields,
        )
    }

    fn add_prepared_document_impl_with_fts(
        &self,
        table: &str,
        doc_id: DocId,
        mut document: Document,
        inserted: uqa_execution::mutation::publication::InsertedIdentity,
        index_fts: bool,
        metadata: Option<uqa_storage::DocumentMetadata>,
    ) -> Result<(), SQLError> {
        let known_new = inserted.is_vacant();
        let Some(table_name) = self
            .try_resolve_table_name(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let Some(t) = self
            .try_table(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let existed = if known_new {
            false
        } else {
            t.document_store
                .read()
                .get(doc_id)
                .map_err(|error| SQLError::Internal(format!("read existing document: {error}")))?
                .is_some()
        };
        // Value-index maintenance: unindex the previous field values
        // (put may replace an existing document), index the new ones.
        // `old_indexed` is `None` exactly when no index is built, so
        // the common path costs one read-lock check. A failed put must
        // leave the value indexes untouched. A known-new document has
        // no previous values to unindex, so its writes skip the per-row
        // storage lookup entirely and only insert the new values.
        let (old_indexed, indexed_fields) = if known_new {
            (None, Self::value_indexes_built_fields(&t))
        } else {
            let old = Self::value_indexes_old_values(&t, doc_id);
            let fields = old.as_ref().map(|old| {
                old.keys()
                    .cloned()
                    .collect::<Vec<uqa_storage::ValueIndexKey>>()
            });
            (old, fields)
        };
        let persistent_indexed =
            self.persistent_value_index_document_values(&table_name, &document)?;
        let new_indexed = indexed_fields
            .map(|fields| {
                if let Some(values) = &persistent_indexed {
                    return Ok(values.clone());
                }
                self.value_index_document_values(&table_name, &fields, &document)
            })
            .transpose()?;
        self.observe_value_index_write(
            &table_name,
            &t,
            doc_id,
            existed,
            old_indexed.as_ref(),
            persistent_indexed.as_ref().or(new_indexed.as_ref()),
        )?;
        if index_fts {
            let text_fields = self.prepared_document_text_fields(table, &document)?;
            self.publish_prepared_document_text(&table_name, &t, doc_id, text_fields, known_new)?;
        }
        let columns = t.columns.read().clone();
        crate::generated::strip_virtual_generated_columns(&columns, &mut document);
        let metadata = match metadata {
            Some(metadata) => metadata,
            None => uqa_storage::DocumentMetadata::with_tuple_xmin(self.tuple_version_xid()?),
        };
        self.advance_next_id(&table_name, doc_id).map_err(|error| {
            uqa_execution::mutation::errors::identifier_storage_error(
                "observe inserted document identity",
                &error,
            )
        })?;
        // An identity no document ever had has no earlier records in the namespace its watermark was read in, so a store that keeps the table under that namespace writes without reading what it replaces.
        let unused = inserted.is_unused().then(|| t.document_id_namespace());
        let mut store = t.document_store.write();
        let stored = uqa_storage::StoredDocument::with_metadata(document, metadata);
        match unused {
            Some(namespace) => store.put_stored_unused(doc_id, stored, namespace),
            None => store.put_stored(doc_id, stored),
        }
        .map_err(|err| crate::table_storage::document_store_write_error(&err))?;
        if let Some(new) = persistent_indexed.as_ref() {
            self.persist_value_indexes_apply_write(&table_name, doc_id, Some(new), unused)?;
        }
        if let Some(new) = new_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, old_indexed.as_ref(), Some(new));
        }
        drop(store);
        let documents = if existed {
            crate::table_storage::DocumentCountChange::Unchanged
        } else {
            crate::table_storage::DocumentCountChange::Added
        };
        self.mark_row_write(&table_name, &t, documents)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        if existed {
            self.note_row_changed(&table_name, doc_id)?;
        } else {
            self.note_row_inserted(&table_name, doc_id)?;
        }
        Ok(())
    }
}
