//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session portal lifecycle shared by PL/pgSQL routine activations.

mod binding;
mod fetch;

use crate::{
    AnalyzerPhase, DocumentStore, Engine, EpochCoordinator, InvertedIndex, MemoryDocumentStore,
    MemoryInvertedIndex, MemoryVectorIndex, QueryRuntime, RelationIdentity, RuntimeExtensions,
    SQLError, SQLResult, SessionPortalCatalogSnapshot, SessionPortalCommandDeclaration,
    SessionPortalData, SessionPortalDeclaration, SessionPortalMaterialization,
    SessionPortalPosition, SessionPortalRestart, SessionPortalSQLFunctionSnapshots,
    SessionPortalState, SessionPortalTableSnapshots, SessionPortalTransactionOverlay,
    SessionPortalViewSnapshots, StorageContext, TableState, Value, VectorIndex,
};
use binding::bind_session_portal_function_relations;
use fetch::{
    ensure_portal_rows_for_fetch, fetch_directional_query_portal, fetch_indices,
    materialize_portal_to_end, select_portal_rows, uses_directional_query_execution,
};
use uqa_planner::{QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::ast::{CursorDirection, FetchCursorStmt};

enum SessionPortalTableDependencies {
    All,
    Exact(std::collections::BTreeSet<RelationIdentity>),
}

impl SessionPortalTableDependencies {
    fn includes(&self, relation: &RelationIdentity) -> bool {
        match self {
            Self::All => true,
            Self::Exact(relations) => relations.contains(relation),
        }
    }

    fn insert(&mut self, relation: RelationIdentity) {
        if let Self::Exact(relations) = self {
            relations.insert(relation);
        }
    }
}

type SessionPortalTableSource = (
    RelationIdentity,
    std::sync::Arc<TableState>,
    std::sync::Arc<TableState>,
);

impl Engine {
    pub(crate) fn allocate_session_portal_name(&self) -> String {
        let mut next = self.session.next_portal_id.lock();
        let name = format!("<unnamed portal {}>", *next);
        *next += 1;
        name
    }

    fn allocate_session_portal_transaction_origin(&self) -> u64 {
        let mut next = self.session.next_portal_transaction_origin.lock();
        let origin = *next;
        *next = next.wrapping_add(1).max(1);
        origin
    }

    pub(crate) fn open_pending_session_portal(
        &self,
        declaration: SessionPortalDeclaration,
    ) -> Result<(), SQLError> {
        let SessionPortalDeclaration {
            name,
            mut query,
            params,
            columns,
            column_types,
            scrollable,
            holdable,
            binary,
        } = declaration;
        bind_session_portal_query_relations(self, &mut query, &std::collections::BTreeSet::new())?;
        super::bind_query_plan_sequence_references(&mut query, &mut |reference| {
            self.try_resolve_sequence_name(reference)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "bind cursor sequence `{reference}` at DECLARE: {error}"
                    ))
                })?
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{reference}\" does not exist"),
                })
        })?;
        let table_dependencies = session_portal_table_dependencies(self, &query)?;
        let snapshot_gate = self
            .row_locks
            .begin_change_snapshot(&self.runtime.cancellation)?;
        let transaction_overlay = self.capture_session_portal_transaction_overlay()?;
        snapshot_gate.baseline()?;
        drop(snapshot_gate);
        let table_sources = {
            let stack = self.session.transactions.lock();
            let fixed_snapshot = stack
                .first()
                .and_then(|frame| frame.fixed_snapshot.as_ref());
            self.capture_session_portal_table_sources(fixed_snapshot, &table_dependencies)
        };
        let table_snapshots = Self::detach_session_portal_table_snapshots(
            table_sources,
            transaction_overlay.as_ref(),
        )?;
        let catalog_snapshot = std::sync::Arc::new(self.durable.snapshot());
        let view_snapshots = std::sync::Arc::new(catalog_snapshot.views.clone());
        let sql_function_snapshots =
            std::sync::Arc::new(catalog_snapshot.sql_user_functions.clone());
        let restart = holdable.then(|| SessionPortalRestart {
            query: query.clone(),
            params: params.clone(),
            table_snapshots: std::sync::Arc::clone(&table_snapshots),
            view_snapshots: std::sync::Arc::clone(&view_snapshots),
            sql_function_snapshots: std::sync::Arc::clone(&sql_function_snapshots),
            catalog_snapshot: std::sync::Arc::clone(&catalog_snapshot),
        });
        let transaction_origin = self.allocate_session_portal_transaction_origin();
        let mut portals = self.session.portals.lock();
        if portals.contains_key(&name) {
            return Err(cursor_error(&name, "already exists", "42P03"));
        }
        portals.insert(
            name,
            SessionPortalState {
                data: SessionPortalData::Pending {
                    query,
                    params,
                    table_snapshots,
                    view_snapshots,
                    sql_function_snapshots,
                    catalog_snapshot,
                    restart,
                },
                columns,
                column_types,
                transaction_origin,
                position: SessionPortalPosition::BeforeFirst,
                scrollable,
                holdable,
                _binary: binary,
            },
        );
        Ok(())
    }

    pub(crate) fn open_pending_command_session_portal(
        &self,
        declaration: SessionPortalCommandDeclaration,
    ) -> Result<(), SQLError> {
        let SessionPortalCommandDeclaration {
            name,
            command,
            params,
            columns,
            column_types,
            scrollable,
            null_returning_values,
        } = declaration;
        let transaction_origin = self.allocate_session_portal_transaction_origin();
        let mut portals = self.session.portals.lock();
        if portals.contains_key(&name) {
            return Err(cursor_error(&name, "already exists", "42P03"));
        }
        portals.insert(
            name,
            SessionPortalState {
                data: SessionPortalData::PendingCommand {
                    command,
                    params,
                    null_returning_values,
                },
                columns,
                column_types,
                transaction_origin,
                position: SessionPortalPosition::BeforeFirst,
                scrollable,
                holdable: false,
                _binary: false,
            },
        );
        Ok(())
    }

    /// Capture the relation and catalog state visible at the start of a SQL statement. A `BEFORE STATEMENT` trigger executes inside the statement's transaction and may therefore change the live engine before the statement evaluates its source query. `PostgreSQL` keeps those changes outside the statement snapshot, so the remaining query work must read through an immutable query engine while trigger and row effects continue to use the live engine.
    pub(crate) fn capture_statement_snapshot_engine(&self) -> Result<Engine, SQLError> {
        let dependencies = SessionPortalTableDependencies::All;
        let snapshot_gate = self
            .row_locks
            .begin_change_snapshot(&self.runtime.cancellation)?;
        let transaction_overlay = self.capture_session_portal_transaction_overlay()?;
        snapshot_gate.baseline()?;
        drop(snapshot_gate);
        let table_sources = {
            let stack = self.session.transactions.lock();
            let fixed_snapshot = stack
                .first()
                .and_then(|frame| frame.fixed_snapshot.as_ref());
            self.capture_session_portal_table_sources(fixed_snapshot, &dependencies)
        };
        let table_snapshots = Self::detach_session_portal_table_snapshots(
            table_sources,
            transaction_overlay.as_ref(),
        )?;
        let catalog_snapshot = std::sync::Arc::new(self.durable.snapshot());
        let view_snapshots = std::sync::Arc::new(catalog_snapshot.views.clone());
        let sql_function_snapshots =
            std::sync::Arc::new(catalog_snapshot.sql_user_functions.clone());
        Ok(self.session_portal_worker_engine(
            table_snapshots,
            view_snapshots,
            sql_function_snapshots,
            catalog_snapshot,
            self.allocate_session_portal_transaction_origin(),
        ))
    }

    pub(crate) fn ensure_session_portal_available(&self, name: &str) -> Result<(), SQLError> {
        if self.session.portals.lock().contains_key(name) {
            return Err(cursor_error(name, "already exists", "42P03"));
        }
        Ok(())
    }

    pub(crate) fn fetch_session_portal(
        &self,
        fetch: &FetchCursorStmt,
    ) -> Result<SQLResult, SQLError> {
        let mut state = self
            .session
            .portals
            .lock()
            .remove(&fetch.name)
            .ok_or_else(|| cursor_error(&fetch.name, "does not exist", "34000"))?;
        let result = (|| {
            if uses_directional_query_execution(&state) {
                return fetch_directional_query_portal(
                    self,
                    &mut state,
                    fetch.direction,
                    fetch.count,
                    fetch.move_only,
                );
            }
            ensure_portal_rows_for_fetch(
                self,
                &mut state,
                fetch.direction,
                fetch.count,
                fetch.move_only,
            )?;
            let indices = fetch_indices(&mut state, fetch.direction, fetch.count, fetch.move_only)?;
            if fetch.move_only {
                return Ok(SQLResult::from_affected(indices.len() as u64));
            }
            select_portal_rows(&mut state, &indices)
        })();
        self.session
            .portals
            .lock()
            .insert(fetch.name.clone(), state);
        result
    }

    pub(crate) fn materialize_holdable_session_portals(&self) -> Result<bool, SQLError> {
        let names = self
            .session
            .portals
            .lock()
            .iter()
            .filter_map(|(name, portal)| {
                (portal.holdable
                    && matches!(
                        &portal.data,
                        SessionPortalData::Pending { .. } | SessionPortalData::Streaming { .. }
                    ))
                .then_some(name.clone())
            })
            .collect::<Vec<_>>();
        let materialized_any = !names.is_empty();
        for name in names {
            let mut state = self
                .session
                .portals
                .lock()
                .remove(&name)
                .ok_or_else(|| cursor_error(&name, "does not exist", "34000"))?;
            let _row_lock_statement = self.begin_row_lock_statement();
            let result = materialize_portal_to_end(self, &mut state);
            self.session.portals.lock().insert(name, state);
            result?;
        }
        Ok(materialized_any)
    }

    pub(crate) fn close_session_portal(&self, name: &str) -> Result<(), SQLError> {
        if self.session.portals.lock().remove(name).is_none() {
            return Err(cursor_error(name, "does not exist", "34000"));
        }
        Ok(())
    }

    pub(crate) fn close_all_session_portals(&self) {
        self.session.portals.lock().clear();
    }

    fn capture_session_portal_table_sources(
        &self,
        fixed_snapshot: Option<&crate::FixedTransactionSnapshot>,
        dependencies: &SessionPortalTableDependencies,
    ) -> Vec<SessionPortalTableSource> {
        let live_tables = self
            .storage
            .tables
            .read()
            .iter()
            .filter(|(relation, _)| dependencies.includes(relation))
            .map(|(relation, metadata)| (relation.clone(), std::sync::Arc::clone(metadata)))
            .collect::<Vec<_>>();
        live_tables
            .into_iter()
            .map(|(relation, metadata)| {
                let data = fixed_snapshot
                    .and_then(|snapshot| snapshot.table_for_live_relation(&relation, &metadata))
                    .unwrap_or_else(|| std::sync::Arc::clone(&metadata));
                (relation, data, metadata)
            })
            .collect()
    }

    fn detach_session_portal_table_snapshots(
        sources: Vec<SessionPortalTableSource>,
        transaction_overlay: &std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<crate::DocId, Option<crate::Document>>,
        >,
    ) -> Result<SessionPortalTableSnapshots, SQLError> {
        let mut snapshots = std::collections::BTreeMap::new();
        for (relation, data, metadata) in sources {
            let canonical = relation.qualified_name();
            let changes = transaction_overlay.get(&canonical);
            snapshots.insert(
                relation,
                Self::detach_query_table(&data, &metadata, changes)?,
            );
        }
        Ok(std::sync::Arc::new(snapshots))
    }

    pub(crate) fn capture_detached_fixed_transaction_snapshot(
        &self,
    ) -> Result<SessionPortalTableSnapshots, SQLError> {
        let mut snapshots = std::collections::BTreeMap::new();
        let live_tables = self
            .storage
            .tables
            .read()
            .iter()
            .filter(|(_, table)| table.persistence != uqa_sql::ast::RelationPersistence::Temporary)
            .map(|(relation, table)| (relation.clone(), std::sync::Arc::clone(table)))
            .collect::<Vec<_>>();
        for (relation, table) in live_tables {
            snapshots.insert(relation, Self::detach_query_table(&table, &table, None)?);
        }
        Ok(std::sync::Arc::new(snapshots))
    }

    fn detached_documents(
        data: &std::sync::Arc<TableState>,
        changes: Option<&std::collections::BTreeMap<crate::DocId, Option<crate::Document>>>,
    ) -> Result<std::collections::BTreeMap<crate::DocId, crate::Document>, SQLError> {
        let source_store = data.document_store.read();
        let doc_ids = source_store
            .doc_ids()
            .map_err(|error| portal_snapshot_error("document ids", &error))?;
        let mut documents = source_store
            .get_many(&doc_ids)
            .map_err(|error| portal_snapshot_error("documents", &error))?;
        drop(source_store);
        if let Some(changes) = changes {
            for (doc_id, document) in changes {
                match document {
                    Some(document) => {
                        documents.insert(*doc_id, document.clone());
                    }
                    None => {
                        documents.remove(doc_id);
                    }
                }
            }
        }
        Ok(documents)
    }

    fn detached_inverted_index(
        data: &std::sync::Arc<TableState>,
        analyzer: &crate::Analyzer,
        fts_fields: &[crate::FieldName],
        documents: &std::collections::BTreeMap<crate::DocId, crate::Document>,
    ) -> Result<MemoryInvertedIndex, SQLError> {
        let mut inverted_index = MemoryInvertedIndex::new(analyzer.clone());
        {
            let source_index = data.inverted_index.read();
            for field in fts_fields {
                inverted_index
                    .set_field_analyzer(
                        field,
                        source_index.get_field_analyzer(field),
                        AnalyzerPhase::Index,
                    )
                    .map_err(|error| portal_snapshot_error("index analyzer", &error))?;
                inverted_index
                    .set_field_analyzer(
                        field,
                        source_index.get_search_analyzer(field),
                        AnalyzerPhase::Search,
                    )
                    .map_err(|error| portal_snapshot_error("search analyzer", &error))?;
            }
        }
        for (doc_id, document) in documents {
            let fields = fts_fields
                .iter()
                .filter_map(|field| match document.get(field) {
                    Some(Value::Str(value)) => Some((field.clone(), value.clone())),
                    _ => None,
                })
                .collect();
            inverted_index
                .add_document(*doc_id, fields)
                .map_err(|error| portal_snapshot_error("inverted index", &error))?;
        }
        Ok(inverted_index)
    }

    fn detached_vector_indexes(
        metadata: &std::sync::Arc<TableState>,
        documents: &std::collections::BTreeMap<crate::DocId, crate::Document>,
    ) -> Result<std::collections::BTreeMap<crate::FieldName, Box<dyn VectorIndex>>, SQLError> {
        let mut vector_indexes: std::collections::BTreeMap<crate::FieldName, Box<dyn VectorIndex>> =
            std::collections::BTreeMap::new();
        for (field, source_index) in metadata.vector_indexes.read().iter() {
            let mut index = MemoryVectorIndex::new(source_index.dimensions());
            for (doc_id, document) in documents {
                let Some(value) = document.get(field) else {
                    continue;
                };
                if let Some(vectors) = Self::field_index_vectors(metadata, field, value)? {
                    index
                        .add_many(*doc_id, vectors)
                        .map_err(|error| portal_snapshot_error("vector index", &error))?;
                }
            }
            vector_indexes.insert(field.clone(), Box::new(index));
        }
        Ok(vector_indexes)
    }

    pub(crate) fn detach_query_table(
        data: &std::sync::Arc<TableState>,
        metadata: &std::sync::Arc<TableState>,
        changes: Option<&std::collections::BTreeMap<crate::DocId, Option<crate::Document>>>,
    ) -> Result<std::sync::Arc<TableState>, SQLError> {
        let documents = Self::detached_documents(data, changes)?;
        Self::detached_query_table_from_documents(data, metadata, &documents)
    }

    pub(crate) fn detach_empty_query_table(
        metadata: &std::sync::Arc<TableState>,
    ) -> Result<std::sync::Arc<TableState>, SQLError> {
        Self::detached_query_table_from_documents(
            metadata,
            metadata,
            &std::collections::BTreeMap::new(),
        )
    }

    fn detached_query_table_from_documents(
        data: &std::sync::Arc<TableState>,
        metadata: &std::sync::Arc<TableState>,
        documents: &std::collections::BTreeMap<crate::DocId, crate::Document>,
    ) -> Result<std::sync::Arc<TableState>, SQLError> {
        let data_columns = data.columns.read().clone();
        let metadata_columns = metadata.columns.read().clone();
        let metadata_by_id = metadata_columns
            .iter()
            .filter_map(|column| column.object_id.map(|object_id| (object_id, column)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut adapted_documents = std::collections::BTreeMap::new();
        for (doc_id, source_document) in documents {
            let mut document = source_document.clone();
            for source_column in &data_columns {
                let target = source_column
                    .object_id
                    .and_then(|object_id| metadata_by_id.get(&object_id).copied())
                    .or_else(|| {
                        metadata_columns.iter().find(|target| {
                            target.name == source_column.name
                                && (source_column.object_id.is_none() || target.object_id.is_none())
                        })
                    });
                let Some(target) = target else {
                    document.remove(&source_column.name);
                    continue;
                };
                if target.name != source_column.name {
                    if let Some(value) = document.remove(&source_column.name) {
                        document.entry(target.name.clone()).or_insert(value);
                    }
                }
            }
            for target in &metadata_columns {
                if target.generated.is_none() && !document.contains_key(&target.name) {
                    document.insert(
                        target.name.clone(),
                        target.missing_value.clone().unwrap_or(Value::Null),
                    );
                }
            }
            crate::engine_generated::materialize_missing_generated_columns(
                &metadata_columns,
                &mut document,
            )?;
            adapted_documents.insert(*doc_id, document);
        }
        let mut document_store = MemoryDocumentStore::new();
        for (doc_id, document) in &adapted_documents {
            document_store
                .put(*doc_id, document.clone())
                .map_err(|error| portal_snapshot_error("memory document", &error))?;
        }
        let analyzer = metadata.analyzer.read().clone();
        let fts_fields = metadata.fts_fields.read().clone();
        let inverted_index =
            Self::detached_inverted_index(metadata, &analyzer, &fts_fields, &adapted_documents)?;
        let vector_indexes = Self::detached_vector_indexes(metadata, &adapted_documents)?;

        Ok(std::sync::Arc::new(TableState {
            lifecycle_id: std::sync::atomic::AtomicU64::new(metadata.lifecycle_id()),
            object_id: metadata.object_id(),
            storage_generation: parking_lot::RwLock::new(metadata.storage_generation()),
            document_store: parking_lot::RwLock::new(Box::new(document_store)),
            inverted_index: parking_lot::RwLock::new(Box::new(inverted_index)),
            vector_indexes: parking_lot::RwLock::new(vector_indexes),
            fts_fields: parking_lot::RwLock::new(fts_fields),
            columns: parking_lot::RwLock::new(metadata_columns),
            next_id: parking_lot::Mutex::new(*metadata.next_id.lock()),
            analyzer: parking_lot::RwLock::new(analyzer),
            column_stats: parking_lot::RwLock::new(metadata.column_stats.read().clone()),
            column_stats_loaded: std::sync::atomic::AtomicBool::new(
                metadata
                    .column_stats_loaded
                    .load(std::sync::atomic::Ordering::Acquire),
            ),
            column_stats_dirty: std::sync::atomic::AtomicBool::new(
                metadata
                    .column_stats_dirty
                    .load(std::sync::atomic::Ordering::Acquire),
            ),
            table_checks: parking_lot::RwLock::new(metadata.table_checks.read().clone()),
            foreign_keys: parking_lot::RwLock::new(metadata.foreign_keys.read().clone()),
            key_constraints: parking_lot::RwLock::new(metadata.key_constraints.read().clone()),
            hierarchy: parking_lot::RwLock::new(metadata.hierarchy.read().clone()),
            value_indexes: parking_lot::RwLock::new(std::collections::BTreeMap::new()),
            doc_count_cache: std::sync::atomic::AtomicU64::new(
                u64::try_from(adapted_documents.len()).unwrap_or(u64::MAX),
            ),
            doc_count_dirty: std::sync::atomic::AtomicBool::new(false),
            persistence: metadata.persistence,
            on_commit: metadata.on_commit,
        }))
    }

    fn capture_session_portal_transaction_overlay(
        &self,
    ) -> Result<SessionPortalTransactionOverlay, SQLError> {
        let relation_names = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| (table.storage_generation(), relation.qualified_name()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let desired = {
            let stack = self.session.transactions.lock();
            let mut desired = std::collections::BTreeMap::<
                String,
                std::collections::BTreeMap<crate::DocId, bool>,
            >::new();
            for change in stack.iter().flat_map(|frame| frame.row_changes.iter()) {
                if let Some(table) = relation_names.get(&change.source_generation) {
                    desired.entry(table.clone()).or_default().insert(
                        change.pending.key.doc_id,
                        !matches!(
                            change.pending.kind,
                            crate::row_locks::PendingRowChangeKind::Delete
                                | crate::row_locks::PendingRowChangeKind::Rewrite(_)
                        ),
                    );
                }
                if let crate::row_locks::PendingRowChangeKind::Rewrite(successor) =
                    change.pending.kind
                {
                    if let Some(table) = change
                        .successor_generation
                        .and_then(|generation| relation_names.get(&generation))
                    {
                        desired
                            .entry(table.clone())
                            .or_default()
                            .insert(successor.doc_id, true);
                    }
                }
            }
            desired
        };
        let mut overlay = std::collections::BTreeMap::new();
        for (table_name, desired_documents) in desired {
            let table = self.require_table(&table_name)?;
            let present = desired_documents
                .iter()
                .filter_map(|(doc_id, present)| present.then_some(*doc_id))
                .collect::<Vec<_>>();
            let documents = table
                .document_store
                .read()
                .get_many(&present)
                .map_err(|error| portal_snapshot_error("transaction documents", &error))?;
            overlay.insert(
                table_name,
                desired_documents
                    .into_iter()
                    .map(|(doc_id, present)| {
                        let document = present.then(|| documents.get(&doc_id).cloned()).flatten();
                        (doc_id, document)
                    })
                    .collect(),
            );
        }
        Ok(std::sync::Arc::new(overlay))
    }
}

mod dependencies;
use dependencies::{bind_session_portal_query_relations, session_portal_table_dependencies};
impl Engine {
    fn session_portal_worker_engine(
        &self,
        table_snapshots: SessionPortalTableSnapshots,
        view_snapshots: SessionPortalViewSnapshots,
        sql_function_snapshots: SessionPortalSQLFunctionSnapshots,
        catalog_snapshot: SessionPortalCatalogSnapshot,
        transaction_origin: u64,
    ) -> Engine {
        let mut epochs = EpochCoordinator::new();
        epochs.share_published_from(&self.epochs);
        let mut runtime = QueryRuntime::new(self.sql_function_depth_limit());
        runtime.statement_gate = std::sync::Arc::clone(&self.runtime.statement_gate);
        runtime.cancellation = self.runtime.cancellation.clone();
        runtime.notices = std::sync::Arc::clone(&self.runtime.notices);
        Engine {
            storage: StorageContext::shared_from(&self.storage),
            durable: std::sync::Arc::clone(&self.durable),
            session: std::sync::Arc::clone(&self.session),
            extensions: RuntimeExtensions::shared_from(&self.extensions),
            epochs,
            runtime,
            row_locks: std::sync::Arc::clone(&self.row_locks),
            session_id: self.session_id,
            owns_session_registration: false,
            query_table_snapshots: Some(table_snapshots),
            query_view_snapshots: Some(view_snapshots),
            query_sql_function_snapshots: Some(sql_function_snapshots),
            query_catalog_snapshot: Some(catalog_snapshot),
            query_transaction_overlay: Some(std::sync::Arc::new(std::collections::BTreeMap::new())),
            query_transaction_origin: Some(transaction_origin),
        }
    }
}

fn cursor_error(name: &str, message: &str, sqlstate: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("cursor \"{name}\" {message}"),
    }
}

fn portal_snapshot_error(component: &str, error: &impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("capture cursor {component} snapshot: {error}"))
}
