//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session portal lifecycle shared by PL/pgSQL routine activations.

use crate::{
    AnalyzerPhase, DocumentStore, Engine, EpochCoordinator, InvertedIndex, MemoryDocumentStore,
    MemoryInvertedIndex, MemoryVectorIndex, QueryRuntime, RelationIdentity, RuntimeExtensions,
    SQLError, SQLResult, SessionPortalCatalogSnapshot, SessionPortalData, SessionPortalDeclaration,
    SessionPortalMaterialization, SessionPortalPosition, SessionPortalRestart,
    SessionPortalSQLFunctionSnapshots, SessionPortalState, SessionPortalTableSnapshots,
    SessionPortalTransactionOverlay, SessionPortalViewSnapshots, StorageContext, TableState, Value,
    VectorIndex,
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

    pub(crate) fn open_session_portal(
        &self,
        name: String,
        result: SQLResult,
    ) -> Result<(), SQLError> {
        self.open_session_portal_with_options(name, result, false, false)
    }

    pub(crate) fn open_session_portal_with_options(
        &self,
        name: String,
        result: SQLResult,
        scrollable: bool,
        holdable: bool,
    ) -> Result<(), SQLError> {
        let columns = result.columns.clone();
        let column_types = result.column_types.clone();
        let mut portals = self.session.portals.lock();
        if portals.contains_key(&name) {
            return Err(cursor_error(&name, "already exists", "42P03"));
        }
        portals.insert(
            name,
            SessionPortalState {
                data: SessionPortalData::Result(result),
                columns,
                column_types,
                transaction_origin: 0,
                position: SessionPortalPosition::BeforeFirst,
                scrollable,
                holdable,
                _binary: false,
            },
        );
        Ok(())
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

    pub(crate) fn ensure_session_portal_available(&self, name: &str) -> Result<(), SQLError> {
        if self.session.portals.lock().contains_key(name) {
            return Err(cursor_error(name, "already exists", "42P03"));
        }
        Ok(())
    }

    pub(crate) fn fetch_session_portal_next(
        &self,
        name: &str,
    ) -> Result<(Vec<String>, Option<Vec<Value>>), SQLError> {
        let result = self.fetch_session_portal(&FetchCursorStmt {
            name: name.to_string(),
            direction: CursorDirection::Forward,
            count: 1,
            move_only: false,
        })?;
        let values = result.rows.first().map(|_| {
            (0..result.columns.len())
                .map(|column| result.value_at(0, column).cloned().unwrap_or(Value::Null))
                .collect()
        });
        Ok((result.columns, values))
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
            ensure_portal_rows_for_fetch(self, &mut state, fetch.direction, fetch.count)?;
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

fn session_portal_table_dependencies(
    engine: &Engine,
    query: &QueryPlan,
) -> Result<SessionPortalTableDependencies, SQLError> {
    let mut dependencies = SessionPortalTableDependencies::Exact(std::collections::BTreeSet::new());
    collect_session_portal_query_dependencies(
        engine,
        query,
        &mut dependencies,
        &mut std::collections::BTreeSet::new(),
        &mut std::collections::BTreeSet::new(),
    )?;
    Ok(dependencies)
}

fn collect_session_portal_query_dependencies(
    engine: &Engine,
    query: &QueryPlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if matches!(dependencies, SessionPortalTableDependencies::All) {
        return Ok(());
    }
    for cte in &query.ctes {
        collect_session_portal_query_dependencies(
            engine,
            &cte.query,
            dependencies,
            visiting_views,
            visiting_routines,
        )?;
    }
    collect_session_portal_relational_dependencies(
        engine,
        &query.root,
        dependencies,
        visiting_views,
        visiting_routines,
    )?;

    let mut plan = uqa_planner::UnifiedPlan::Query(Box::new(query.clone()));
    let mut routines = Vec::new();
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let uqa_execution::ScalarExpr::Func { name, binding, .. } = expression {
            routines.push((name.clone(), binding.clone()));
        }
    });
    for (name, binding) in routines {
        collect_session_portal_routine_dependencies(
            engine,
            &name,
            binding.as_ref(),
            dependencies,
            visiting_views,
            visiting_routines,
        )?;
        if matches!(dependencies, SessionPortalTableDependencies::All) {
            break;
        }
    }
    Ok(())
}

fn collect_session_portal_relational_dependencies(
    engine: &Engine,
    plan: &RelationalPlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                collect_session_portal_source_dependencies(
                    engine,
                    source,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            for subquery in &block.subqueries {
                collect_session_portal_query_dependencies(
                    engine,
                    subquery,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            collect_session_portal_query_dependencies(
                engine,
                left,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            collect_session_portal_query_dependencies(
                engine,
                right,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            for subquery in subqueries {
                collect_session_portal_query_dependencies(
                    engine,
                    subquery,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                collect_session_portal_query_dependencies(
                    engine,
                    subquery,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
        }
    }
    Ok(())
}

fn collect_session_portal_source_dependencies(
    engine: &Engine,
    source: &SourcePlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table {
            name,
            include_descendants,
            ..
        } => {
            if let Some(table) = engine.try_resolve_table_name(name).map_err(|error| {
                SQLError::Internal(format!(
                    "resolve cursor dependency relation `{name}`: {error}"
                ))
            })? {
                for table in engine.hierarchy_scan_tables(&table, *include_descendants)? {
                    dependencies.insert(Engine::resolved_relation_identity(&table).map_err(
                        |error| {
                            SQLError::Internal(format!(
                                "resolve cursor dependency identity `{table}`: {error}"
                            ))
                        },
                    )?);
                }
                return Ok(());
            }
            if super::canonical_virtual_relation_reference(name).is_some() {
                *dependencies = SessionPortalTableDependencies::All;
                return Ok(());
            }
            let key = name.to_ascii_lowercase();
            if !visiting_views.insert(key.clone()) {
                return Ok(());
            }
            if let Some(view) = engine.view_plan(name)? {
                collect_session_portal_query_dependencies(
                    engine,
                    &view,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            visiting_views.remove(&key);
            Ok(())
        }
        SourcePlan::Join { left, right, .. } => {
            collect_session_portal_source_dependencies(
                engine,
                left,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            collect_session_portal_source_dependencies(
                engine,
                right,
                dependencies,
                visiting_views,
                visiting_routines,
            )
        }
        SourcePlan::Subquery { body, .. } => collect_session_portal_query_dependencies(
            engine,
            body,
            dependencies,
            visiting_views,
            visiting_routines,
        ),
        SourcePlan::Function {
            name,
            binding,
            relation,
            ..
        } => collect_session_portal_function_dependencies(
            engine,
            name,
            binding.as_ref(),
            relation.as_deref(),
            dependencies,
            visiting_views,
            visiting_routines,
        ),
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                collect_session_portal_function_dependencies(
                    engine,
                    &function.name,
                    function.binding.as_ref(),
                    function.relation.as_deref(),
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            Ok(())
        }
        SourcePlan::Values { .. } => Ok(()),
    }
}

fn collect_session_portal_function_dependencies(
    engine: &Engine,
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    relation: Option<&str>,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if let Some(relation) = relation {
        collect_session_portal_function_relation_dependency(engine, relation, dependencies)?;
    }
    collect_session_portal_routine_dependencies(
        engine,
        name,
        binding,
        dependencies,
        visiting_views,
        visiting_routines,
    )
}

fn collect_session_portal_function_relation_dependency(
    engine: &Engine,
    name: &str,
    dependencies: &mut SessionPortalTableDependencies,
) -> Result<(), SQLError> {
    let Some(table) = engine.try_resolve_table_name(name).map_err(|error| {
        SQLError::Internal(format!(
            "resolve cursor table-function relation `{name}`: {error}"
        ))
    })?
    else {
        return Ok(());
    };
    dependencies.insert(Engine::resolved_relation_identity(&table).map_err(|error| {
        SQLError::Internal(format!(
            "resolve cursor table-function relation identity `{table}`: {error}"
        ))
    })?);
    Ok(())
}

fn collect_session_portal_routine_dependencies(
    engine: &Engine,
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if binding.is_some_and(|binding| binding.builtin) {
        return Ok(());
    }
    let lookup_name = binding.map_or(name, |binding| binding.name.as_str());
    let Some(overloads) = engine.lookup_sql_functions(lookup_name) else {
        return Ok(());
    };
    for function in overloads {
        if function.def.is_procedure
            || binding.is_some_and(|binding| {
                crate::engine_user_functions::routine_signature_types(&function.def)
                    != binding.argument_types
            })
        {
            continue;
        }
        let signature =
            crate::engine_user_functions::routine_signature_types(&function.def).join(",");
        let key = format!("{}({signature})", function.def.name);
        if !visiting_routines.insert(key.clone()) {
            continue;
        }
        match &function.compiled {
            crate::engine_user_functions::CompiledFunctionBody::SQL(plans) => {
                for plan in plans {
                    match plan {
                        uqa_planner::UnifiedPlan::Query(query) => {
                            collect_session_portal_query_dependencies(
                                engine,
                                query,
                                dependencies,
                                visiting_views,
                                visiting_routines,
                            )?;
                        }
                        uqa_planner::UnifiedPlan::Command(_) => {
                            *dependencies = SessionPortalTableDependencies::All;
                        }
                    }
                    if matches!(dependencies, SessionPortalTableDependencies::All) {
                        break;
                    }
                }
            }
            crate::engine_user_functions::CompiledFunctionBody::PLpgSQL(_) => {
                *dependencies = SessionPortalTableDependencies::All;
            }
        }
        visiting_routines.remove(&key);
        if matches!(dependencies, SessionPortalTableDependencies::All) {
            break;
        }
    }
    Ok(())
}

fn bind_session_portal_query_relations(
    engine: &Engine,
    query: &mut QueryPlan,
    inherited_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut visible_ctes = inherited_ctes.clone();
    for cte in &mut query.ctes {
        let mut definition_scope = visible_ctes.clone();
        if cte.recursive {
            definition_scope.insert(cte.name.clone());
        }
        bind_session_portal_query_relations(engine, &mut cte.query, &definition_scope)?;
        visible_ctes.insert(cte.name.clone());
    }
    bind_session_portal_relational_plan(engine, &mut query.root, &visible_ctes)
}

fn bind_session_portal_relational_plan(
    engine: &Engine,
    plan: &mut RelationalPlan,
    visible_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_mut() {
                bind_session_portal_source_plan(engine, source, visible_ctes)?;
            }
            for subquery in &mut block.subqueries {
                bind_session_portal_query_relations(engine, subquery, visible_ctes)?;
            }
            Ok(())
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            bind_session_portal_query_relations(engine, left, visible_ctes)?;
            bind_session_portal_query_relations(engine, right, visible_ctes)?;
            for subquery in subqueries {
                bind_session_portal_query_relations(engine, subquery, visible_ctes)?;
            }
            Ok(())
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                bind_session_portal_query_relations(engine, subquery, visible_ctes)?;
            }
            Ok(())
        }
    }
}

fn bind_session_portal_source_plan(
    engine: &Engine,
    source: &mut SourcePlan,
    visible_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table { name, .. } => {
            if visible_ctes.contains(name) {
                return Ok(());
            }
            let requested = name.clone();
            if let Some(canonical) = engine.try_resolve_table_name(&requested).map_err(|error| {
                SQLError::Internal(format!(
                    "bind cursor relation `{requested}` at DECLARE: {error}"
                ))
            })? {
                *name = canonical;
            } else if let Some(canonical) =
                engine.try_resolve_view_name(&requested).map_err(|error| {
                    SQLError::Internal(format!(
                        "bind cursor view `{requested}` at DECLARE: {error}"
                    ))
                })?
            {
                *name = canonical;
            }
            Ok(())
        }
        SourcePlan::Join { left, right, .. } => {
            bind_session_portal_source_plan(engine, left, visible_ctes)?;
            bind_session_portal_source_plan(engine, right, visible_ctes)
        }
        SourcePlan::Subquery { body, .. } => {
            bind_session_portal_query_relations(engine, body, visible_ctes)
        }
        SourcePlan::Function { relation, .. } => {
            bind_session_portal_function_relation(engine, relation)
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                bind_session_portal_function_relation(engine, &mut function.relation)?;
            }
            Ok(())
        }
        SourcePlan::Values { .. } => Ok(()),
    }
}

fn bind_session_portal_function_relation(
    engine: &Engine,
    relation: &mut Option<String>,
) -> Result<(), SQLError> {
    let Some(requested) = relation.clone() else {
        return Ok(());
    };
    if let Some(canonical) = engine.try_resolve_table_name(&requested).map_err(|error| {
        SQLError::Internal(format!(
            "bind cursor table-function relation `{requested}` at DECLARE: {error}"
        ))
    })? {
        *relation = Some(canonical);
    }
    Ok(())
}

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

fn start_streaming_portal(engine: &Engine, state: &mut SessionPortalState) {
    let pending = std::mem::replace(
        &mut state.data,
        SessionPortalData::Result(SQLResult::empty()),
    );
    let SessionPortalData::Pending {
        query,
        params,
        table_snapshots,
        view_snapshots,
        sql_function_snapshots,
        catalog_snapshot,
        restart,
    } = pending
    else {
        state.data = pending;
        return;
    };
    let worker_engine = engine.session_portal_worker_engine(
        table_snapshots,
        view_snapshots,
        sql_function_snapshots,
        catalog_snapshot,
        state.transaction_origin,
    );
    state.data = SessionPortalData::Streaming {
        worker: crate::sql::start_session_portal_worker(worker_engine, query, params),
        materialized: None,
        eof: false,
        restart,
    };
}

fn stream_next_portal_row(
    engine: &Engine,
    state: &mut SessionPortalState,
) -> Result<bool, SQLError> {
    start_streaming_portal(engine, state);
    let SessionPortalData::Streaming {
        worker,
        materialized,
        eof,
        ..
    } = &mut state.data
    else {
        return Ok(false);
    };
    if *eof {
        return Ok(false);
    }
    engine.cancellation_token().check()?;
    let _ = worker
        .requests
        .send(crate::SessionPortalWorkerRequest::Next);
    loop {
        match worker.responses.recv() {
            Ok(crate::SessionPortalWorkerResponse::Started {
                columns,
                column_types,
            }) => {
                let schema =
                    uqa_execution::RowSchema::with_types(columns.clone(), column_types.clone());
                let rows = uqa_execution::IndexedSpill::new(schema)
                    .map_err(crate::sql::map_physical_exec_error)?;
                *materialized = Some(SessionPortalMaterialization {
                    columns,
                    column_types,
                    rows,
                });
            }
            Ok(crate::SessionPortalWorkerResponse::Row(values)) => {
                let output = materialized.as_mut().ok_or_else(|| {
                    SQLError::Internal("cursor worker returned a row before metadata".into())
                })?;
                output
                    .rows
                    .push(&uqa_execution::PhysicalRow::from_values(values))
                    .map_err(crate::sql::map_physical_exec_error)?;
                return Ok(true);
            }
            Ok(crate::SessionPortalWorkerResponse::Eof) => {
                *eof = true;
                return Ok(false);
            }
            Ok(crate::SessionPortalWorkerResponse::Error(error)) => {
                *eof = true;
                return Err(error);
            }
            Err(_) => {
                *eof = true;
                return Err(SQLError::Internal(
                    "cursor worker stopped without completing the query".into(),
                ));
            }
        }
    }
}

fn ensure_portal_rows_for_fetch(
    engine: &Engine,
    state: &mut SessionPortalState,
    mut direction: CursorDirection,
    mut count: i64,
) -> Result<(), SQLError> {
    if count < 0
        && matches!(
            direction,
            CursorDirection::Forward | CursorDirection::Backward
        )
    {
        count = count.checked_neg().ok_or_else(|| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?;
        direction = match direction {
            CursorDirection::Forward => CursorDirection::Backward,
            CursorDirection::Backward => CursorDirection::Forward,
            _ => unreachable!(),
        };
    }
    if count == 0 {
        return Ok(());
    }
    let required = match direction {
        CursorDirection::Forward if count == i64::MAX => None,
        CursorDirection::Forward => {
            let start = match state.position {
                SessionPortalPosition::BeforeFirst => 0,
                SessionPortalPosition::OnRow(position) => position,
                SessionPortalPosition::AfterLast => return Ok(()),
            };
            Some(
                start.saturating_add(usize::try_from(count).map_err(|_| SQLError::Routine {
                    sqlstate: "22003".into(),
                    message: "cursor position is out of range".into(),
                })?),
            )
        }
        CursorDirection::Absolute if count < 0 => {
            require_scroll(state)?;
            None
        }
        CursorDirection::Absolute => {
            Some(usize::try_from(count).map_err(|_| SQLError::Routine {
                sqlstate: "22003".into(),
                message: "cursor position is out of range".into(),
            })?)
        }
        CursorDirection::Relative if count > 0 => {
            let current = match state.position {
                SessionPortalPosition::BeforeFirst => 0,
                SessionPortalPosition::OnRow(position) => position,
                SessionPortalPosition::AfterLast => return Ok(()),
            };
            Some(
                current.saturating_add(usize::try_from(count).map_err(|_| SQLError::Routine {
                    sqlstate: "22003".into(),
                    message: "cursor position is out of range".into(),
                })?),
            )
        }
        CursorDirection::Backward | CursorDirection::Relative => return Ok(()),
    };
    loop {
        if required.is_some_and(|required| portal_row_count(state) >= required) {
            return Ok(());
        }
        if !stream_next_portal_row(engine, state)? {
            return Ok(());
        }
    }
}

fn materialize_portal_to_end(
    engine: &Engine,
    state: &mut SessionPortalState,
) -> Result<(), SQLError> {
    let streaming = std::mem::replace(
        &mut state.data,
        SessionPortalData::Result(SQLResult::empty()),
    );
    state.data = match streaming {
        SessionPortalData::Streaming {
            restart: Some(restart),
            ..
        } => SessionPortalData::Pending {
            query: restart.query,
            params: restart.params,
            table_snapshots: restart.table_snapshots,
            view_snapshots: restart.view_snapshots,
            sql_function_snapshots: restart.sql_function_snapshots,
            catalog_snapshot: restart.catalog_snapshot,
            restart: None,
        },
        other => other,
    };
    while stream_next_portal_row(engine, state)? {}
    let streaming = std::mem::replace(
        &mut state.data,
        SessionPortalData::Result(SQLResult::empty()),
    );
    match streaming {
        SessionPortalData::Streaming {
            materialized: Some(materialized),
            ..
        } => state.data = SessionPortalData::Indexed(materialized),
        SessionPortalData::Streaming {
            materialized: None, ..
        } => {
            return Err(SQLError::Internal(
                "cursor worker completed without result metadata".into(),
            ));
        }
        other => state.data = other,
    }
    Ok(())
}

fn portal_row_count(state: &SessionPortalState) -> usize {
    match &state.data {
        SessionPortalData::Pending { .. } => 0,
        SessionPortalData::Result(result) => result.rows.len(),
        SessionPortalData::Indexed(result) => {
            usize::try_from(result.rows.len()).unwrap_or(usize::MAX)
        }
        SessionPortalData::Streaming { materialized, .. } => {
            materialized.as_ref().map_or(0, |result| {
                usize::try_from(result.rows.len()).unwrap_or(usize::MAX)
            })
        }
    }
}

fn fetch_indices(
    state: &mut SessionPortalState,
    mut direction: CursorDirection,
    mut count: i64,
    move_only: bool,
) -> Result<Vec<usize>, SQLError> {
    if count < 0
        && matches!(
            direction,
            CursorDirection::Forward | CursorDirection::Backward
        )
    {
        count = count.checked_neg().ok_or_else(|| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?;
        direction = match direction {
            CursorDirection::Forward => CursorDirection::Backward,
            CursorDirection::Backward => CursorDirection::Forward,
            _ => unreachable!(),
        };
    }
    // PostgreSQL answers MOVE {FORWARD|BACKWARD|RELATIVE} 0 from the current portal state without asking the executor to rescan. It therefore works for NO SCROLL cursors and reports 1 exactly when the cursor is on a row.
    if move_only
        && count == 0
        && matches!(
            direction,
            CursorDirection::Forward | CursorDirection::Backward | CursorDirection::Relative
        )
    {
        return Ok(current_row(state).into_iter().collect());
    }
    match direction {
        CursorDirection::Forward => fetch_forward(state, count),
        CursorDirection::Backward => fetch_backward(state, count),
        CursorDirection::Absolute => fetch_absolute(state, count),
        CursorDirection::Relative => fetch_relative(state, count),
    }
}

fn require_scroll(state: &SessionPortalState) -> Result<(), SQLError> {
    if state.scrollable {
        Ok(())
    } else {
        Err(SQLError::Routine {
            sqlstate: "55000".into(),
            message: "cursor can only scan forward".into(),
        })
    }
}

fn current_row(state: &SessionPortalState) -> Option<usize> {
    match state.position {
        SessionPortalPosition::OnRow(position) => Some(position - 1),
        SessionPortalPosition::BeforeFirst | SessionPortalPosition::AfterLast => None,
    }
}

fn fetch_forward(state: &mut SessionPortalState, count: i64) -> Result<Vec<usize>, SQLError> {
    if count == 0 {
        if current_row(state).is_some() {
            require_scroll(state)?;
        }
        return Ok(current_row(state).into_iter().collect());
    }
    if state.position == SessionPortalPosition::AfterLast {
        return Ok(Vec::new());
    }
    let start = match state.position {
        SessionPortalPosition::BeforeFirst => 0,
        SessionPortalPosition::OnRow(position) => position,
        SessionPortalPosition::AfterLast => unreachable!(),
    };
    let available = portal_row_count(state).saturating_sub(start);
    let requested = if count == i64::MAX {
        available
    } else {
        usize::try_from(count).map_err(|_| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?
    };
    let fetched = available.min(requested);
    if fetched != 0 {
        state.position = SessionPortalPosition::OnRow(start + fetched);
    }
    if count == i64::MAX || fetched < requested {
        state.position = SessionPortalPosition::AfterLast;
    }
    Ok((start..start + fetched).collect())
}

fn fetch_backward(state: &mut SessionPortalState, count: i64) -> Result<Vec<usize>, SQLError> {
    require_scroll(state)?;
    if count == 0 {
        return Ok(current_row(state).into_iter().collect());
    }
    if state.position == SessionPortalPosition::BeforeFirst {
        return Ok(Vec::new());
    }
    let conceptual_position = match state.position {
        SessionPortalPosition::BeforeFirst => unreachable!(),
        SessionPortalPosition::OnRow(position) => position,
        SessionPortalPosition::AfterLast => portal_row_count(state).saturating_add(1),
    };
    let available = conceptual_position.saturating_sub(1);
    let requested = if count == i64::MAX {
        available
    } else {
        usize::try_from(count).map_err(|_| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?
    };
    let fetched = available.min(requested);
    let indices = (0..fetched)
        .map(|offset| conceptual_position - offset - 2)
        .collect::<Vec<_>>();
    if fetched != 0 {
        state.position = SessionPortalPosition::OnRow(conceptual_position - fetched);
    }
    if count == i64::MAX || fetched < requested {
        state.position = SessionPortalPosition::BeforeFirst;
    }
    Ok(indices)
}

fn fetch_absolute(state: &mut SessionPortalState, count: i64) -> Result<Vec<usize>, SQLError> {
    let row_count = i128::try_from(portal_row_count(state)).unwrap_or(i128::MAX);
    let target = match count.cmp(&0) {
        std::cmp::Ordering::Greater => i128::from(count),
        std::cmp::Ordering::Less => row_count + 1 + i128::from(count),
        std::cmp::Ordering::Equal => 0,
    };
    let current = match state.position {
        SessionPortalPosition::BeforeFirst => 0,
        SessionPortalPosition::OnRow(position) => i128::try_from(position).unwrap_or(i128::MAX),
        SessionPortalPosition::AfterLast => row_count + 1,
    };
    let requires_scroll = count < 0
        || (count == 0 && state.position != SessionPortalPosition::BeforeFirst)
        || (count > 0 && target <= current);
    if !state.scrollable && requires_scroll {
        return require_scroll(state).map(|()| Vec::new());
    }
    position_at(state, target, row_count)
}

fn fetch_relative(state: &mut SessionPortalState, count: i64) -> Result<Vec<usize>, SQLError> {
    let row_count = i128::try_from(portal_row_count(state)).unwrap_or(i128::MAX);
    let current = match state.position {
        SessionPortalPosition::BeforeFirst => 0,
        SessionPortalPosition::OnRow(position) => i128::try_from(position).unwrap_or(i128::MAX),
        SessionPortalPosition::AfterLast => row_count + 1,
    };
    if count == 0 {
        return fetch_forward(state, 0);
    }
    if count < 0 {
        require_scroll(state)?;
    }
    position_at(state, current + i128::from(count), row_count)
}

fn position_at(
    state: &mut SessionPortalState,
    target: i128,
    row_count: i128,
) -> Result<Vec<usize>, SQLError> {
    if target <= 0 {
        state.position = SessionPortalPosition::BeforeFirst;
        return Ok(Vec::new());
    }
    if target > row_count {
        state.position = SessionPortalPosition::AfterLast;
        return Ok(Vec::new());
    }
    let row = usize::try_from(target - 1).map_err(|_| SQLError::Routine {
        sqlstate: "22003".into(),
        message: "cursor position is out of range".into(),
    })?;
    state.position = SessionPortalPosition::OnRow(row + 1);
    Ok(vec![row])
}

fn select_portal_rows(
    state: &mut SessionPortalState,
    indices: &[usize],
) -> Result<SQLResult, SQLError> {
    let empty_columns = state.columns.clone();
    let empty_column_types = state.column_types.clone();
    let empty = || {
        SQLResult::from_typed_rows_with_positions(
            empty_columns.clone(),
            empty_column_types.clone(),
            Vec::new(),
            Some(Vec::new()),
        )
    };
    match &mut state.data {
        SessionPortalData::Pending { .. }
        | SessionPortalData::Streaming {
            materialized: None, ..
        } if indices.is_empty() => Ok(empty()),
        SessionPortalData::Pending { .. } => Err(SQLError::Internal(
            "session portal remained pending during FETCH".into(),
        )),
        SessionPortalData::Result(result) => Ok(select_result_rows(result, indices)),
        SessionPortalData::Indexed(result)
        | SessionPortalData::Streaming {
            materialized: Some(result),
            ..
        } => select_indexed_rows(result, indices),
        SessionPortalData::Streaming { .. } => Err(SQLError::Internal(
            "cursor row materialization is absent".into(),
        )),
    }
}

fn select_result_rows(result: &SQLResult, indices: &[usize]) -> SQLResult {
    let rows = indices
        .iter()
        .map(|&index| result.rows[index].clone())
        .collect();
    let positional_rows = result.positional_rows.as_ref().map(|rows| {
        indices
            .iter()
            .map(|&index| rows[index].clone())
            .collect::<Vec<_>>()
    });
    SQLResult {
        columns: result.columns.clone(),
        column_types: result.column_types.clone(),
        rows,
        positional_rows,
        affected_rows: 0,
    }
}

fn select_indexed_rows(
    result: &mut SessionPortalMaterialization,
    indices: &[usize],
) -> Result<SQLResult, SQLError> {
    let schema = result.rows.row_schema().clone();
    let mut positional_rows = Vec::with_capacity(indices.len());
    for &index in indices {
        let index = u64::try_from(index).map_err(|_| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?;
        let row = result
            .rows
            .get(index)
            .map_err(crate::sql::map_physical_exec_error)?;
        let view = schema.view(&row);
        positional_rows.push(
            (0..result.columns.len())
                .map(|position| view.value_at(position).cloned().unwrap_or(Value::Null))
                .collect(),
        );
    }
    Ok(SQLResult::from_typed_rows_with_positions(
        result.columns.clone(),
        result.column_types.clone(),
        vec![uqa_sql::ResultRow::new(); positional_rows.len()],
        Some(positional_rows),
    ))
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
