//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog facade implementation for key/value-backed persistence.

use std::collections::BTreeSet;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    GraphSnapshot, RelationIdentity, RelationKind, SequenceRow, TableSchema, ViewRow,
};
use crate::{StorageBackendError, StorageBackendResult};

use super::{
    decode_document_value, decode_string, decode_value, doc_length_key, doc_length_key_prefix,
    document_key_prefix, encode_document_value, encode_value, field_stats_key,
    field_stats_key_prefix, key_with_tag, posting_field_prefix, posting_key_prefix, push_str,
    push_u64, read_str, read_u64, reverse_posting_key, reverse_posting_key_prefix, single_str_key,
    string_value, vector_field_prefix, vector_key_prefix, KeyValueBatch, KeyValueStore,
    TAG_ANALYZER, TAG_CATALOG_INDEX, TAG_COLUMN_STATS, TAG_EDGE, TAG_FOREIGN_SERVER,
    TAG_FOREIGN_TABLE, TAG_GRAPH_MEMBERSHIP, TAG_METADATA, TAG_MODEL, TAG_NAMED_GRAPH,
    TAG_PATH_INDEX, TAG_RELATION, TAG_SCHEMA, TAG_SCORING_PARAMS, TAG_SEQUENCE, TAG_TABLE,
    TAG_TABLE_FIELD_ANALYZER, TAG_VERTEX, TAG_VIEW,
};

const LEGACY_VIEWS_METADATA_KEY: &str = "sql_views_json";
const LEGACY_SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";

#[derive(Debug, Serialize, Deserialize)]
struct StoredVertex {
    label: String,
    properties_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredEdge {
    source_id: u64,
    target_id: u64,
    label: String,
    properties_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredForeignServer {
    fdw_type: String,
    options_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredForeignTable {
    server_name: String,
    columns_json: String,
    options_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredRelation {
    kind: RelationKind,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredView {
    definition_json: String,
}

#[derive(Debug, Deserialize)]
struct LegacyTableSchema {
    name: String,
    analyzer_json: String,
    fts_fields: Vec<String>,
    vector_fields: Vec<crate::catalog::VectorFieldSchema>,
    #[serde(default)]
    columns_json: String,
    #[serde(default)]
    constraints_json: String,
}

#[derive(Debug, Deserialize)]
struct LegacySequenceState {
    start: i64,
    increment: i64,
    current: i64,
}

fn relation_key(tag: u8, relation: &RelationIdentity) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(tag);
    push_str(&mut key, &relation.schema)?;
    push_str(&mut key, &relation.name)?;
    Ok(key)
}

fn decode_relation_key(key: &[u8]) -> StorageBackendResult<RelationIdentity> {
    let mut offset = 1;
    let schema = read_str(key, &mut offset)?;
    let name = read_str(key, &mut offset)?;
    if offset != key.len() {
        return Err(StorageBackendError::Other(
            "corrupt relation catalog key has trailing bytes".into(),
        ));
    }
    Ok(RelationIdentity::new(schema, name))
}

fn decode_catalog_relation_key(
    key: &[u8],
) -> StorageBackendResult<(RelationIdentity, bool, String)> {
    let mut offset = 1;
    let first = read_str(key, &mut offset)?;
    if offset == key.len() {
        let relation =
            RelationIdentity::from_legacy_name(&first).map_err(StorageBackendError::Other)?;
        return Ok((relation, true, first));
    }
    let second = read_str(key, &mut offset)?;
    if offset != key.len() {
        return Err(StorageBackendError::Other(
            "corrupt relation catalog key has trailing bytes".into(),
        ));
    }
    let relation = RelationIdentity::new(first, second);
    Ok((relation.clone(), false, relation.qualified_name()))
}

fn register_migration_relation(
    seen: &mut std::collections::BTreeMap<RelationIdentity, (RelationKind, String)>,
    relation: &RelationIdentity,
    kind: RelationKind,
    source: String,
) -> StorageBackendResult<()> {
    if let Some((existing_kind, existing_source)) = seen.get(relation) {
        return Err(StorageBackendError::Other(format!(
            "relation namespace migration collision for `{}`: {} `{}` and {} `{}`",
            relation.qualified_name(),
            existing_kind.as_str(),
            existing_source,
            kind.as_str(),
            source
        )));
    }
    seen.insert(relation.clone(), (kind, source));
    Ok(())
}

fn ensure_prefix_absent(
    store: &dyn KeyValueStore,
    prefix: &[u8],
    relation: &RelationIdentity,
) -> StorageBackendResult<()> {
    if !store.scan_prefix(prefix)?.is_empty() {
        return Err(StorageBackendError::Other(format!(
            "relation namespace migration for `{}` would overwrite existing table data",
            relation.qualified_name()
        )));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredCatalogIndex {
    index_type: String,
    table_name: String,
    columns_json: String,
    parameters_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredColumnStats {
    distinct_count: i64,
    null_count: i64,
    min_value: Option<String>,
    max_value: Option<String>,
    row_count: i64,
    histogram_json: String,
    mcv_values_json: String,
    mcv_frequencies_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredSequence {
    start: i64,
    increment: i64,
    current: i64,
    #[serde(default = "legacy_sequence_called")]
    called: bool,
}

const fn legacy_sequence_called() -> bool {
    true
}

type SeenRelations = std::collections::BTreeMap<RelationIdentity, (RelationKind, String)>;

struct TableMigration {
    old_key: Vec<u8>,
    old_physical_name: String,
    schema: TableSchema,
}

struct SequenceMigration {
    old_key: Option<Vec<u8>>,
    row: SequenceRow,
}

struct ForeignMigration {
    old_key: Vec<u8>,
    relation: RelationIdentity,
    value: Vec<u8>,
}

struct ViewMigration {
    old_key: Option<Vec<u8>>,
    row: ViewRow,
}

struct RelationMigrations {
    seen: SeenRelations,
    tables: Vec<TableMigration>,
    sequences: Vec<SequenceMigration>,
    foreign_tables: Vec<ForeignMigration>,
    views: Vec<ViewMigration>,
}

fn decode_migrated_table(
    key_relation: &RelationIdentity,
    source: &str,
    value: &[u8],
) -> StorageBackendResult<TableSchema> {
    if let Ok(current) = serde_json::from_slice::<TableSchema>(value) {
        if current.relation != *key_relation {
            return Err(StorageBackendError::Other(format!(
                "table catalog key `{source}` disagrees with stored relation `{}`",
                current.relation.qualified_name()
            )));
        }
        return Ok(current);
    }
    let legacy = serde_json::from_slice::<LegacyTableSchema>(value)?;
    let relation =
        RelationIdentity::from_legacy_name(&legacy.name).map_err(StorageBackendError::Other)?;
    if relation != *key_relation {
        return Err(StorageBackendError::Other(format!(
            "table catalog key `{source}` disagrees with stored name `{}`",
            legacy.name
        )));
    }
    Ok(TableSchema {
        relation,
        analyzer_json: legacy.analyzer_json,
        fts_fields: legacy.fts_fields,
        vector_fields: legacy.vector_fields,
        columns_json: legacy.columns_json,
        constraints_json: legacy.constraints_json,
    })
}

fn collect_table_migrations(
    store: &dyn KeyValueStore,
    seen: &mut SeenRelations,
) -> StorageBackendResult<Vec<TableMigration>> {
    let mut tables = Vec::new();
    for (key, value) in store.scan_prefix(&key_with_tag(TAG_TABLE))? {
        let (key_relation, legacy_key, source) = decode_catalog_relation_key(&key)?;
        let schema = decode_migrated_table(&key_relation, &source, &value)?;
        register_migration_relation(seen, &schema.relation, RelationKind::Table, source.clone())?;
        tables.push(TableMigration {
            old_key: key,
            old_physical_name: if legacy_key {
                source
            } else {
                schema.relation.qualified_name()
            },
            schema,
        });
    }
    Ok(tables)
}

fn collect_sequence_migrations(
    catalog: &KeyValueCatalog,
    seen: &mut SeenRelations,
) -> StorageBackendResult<Vec<SequenceMigration>> {
    let mut sequences = Vec::new();
    for (key, value) in catalog.store.scan_prefix(&key_with_tag(TAG_SEQUENCE))? {
        let (relation, _, source) = decode_catalog_relation_key(&key)?;
        let stored = decode_value::<StoredSequence>(&value)?;
        register_migration_relation(seen, &relation, RelationKind::Sequence, source)?;
        sequences.push(SequenceMigration {
            old_key: Some(key),
            row: SequenceRow {
                relation,
                start: stored.start,
                increment: stored.increment,
                current: stored.current,
                called: stored.called,
            },
        });
    }
    if let Some(json) = catalog.get_metadata(LEGACY_SEQUENCES_METADATA_KEY)? {
        let legacy =
            serde_json::from_str::<std::collections::BTreeMap<String, LegacySequenceState>>(&json)?;
        for (name, state) in legacy {
            let relation =
                RelationIdentity::from_legacy_name(&name).map_err(StorageBackendError::Other)?;
            register_migration_relation(
                seen,
                &relation,
                RelationKind::Sequence,
                format!("legacy metadata `{name}`"),
            )?;
            sequences.push(SequenceMigration {
                old_key: None,
                row: SequenceRow {
                    relation,
                    start: state.start,
                    increment: state.increment,
                    current: state.current,
                    called: true,
                },
            });
        }
    }
    Ok(sequences)
}

fn collect_foreign_migrations(
    store: &dyn KeyValueStore,
    seen: &mut SeenRelations,
) -> StorageBackendResult<Vec<ForeignMigration>> {
    let mut rows = Vec::new();
    for (key, value) in store.scan_prefix(&key_with_tag(TAG_FOREIGN_TABLE))? {
        let (relation, _, source) = decode_catalog_relation_key(&key)?;
        decode_value::<StoredForeignTable>(&value)?;
        register_migration_relation(seen, &relation, RelationKind::ForeignTable, source)?;
        rows.push(ForeignMigration {
            old_key: key,
            relation,
            value,
        });
    }
    Ok(rows)
}

fn collect_view_migrations(
    catalog: &KeyValueCatalog,
    seen: &mut SeenRelations,
) -> StorageBackendResult<Vec<ViewMigration>> {
    let mut views = Vec::new();
    for (key, value) in catalog.store.scan_prefix(&key_with_tag(TAG_VIEW))? {
        let (relation, _, source) = decode_catalog_relation_key(&key)?;
        let stored = decode_value::<StoredView>(&value)?;
        register_migration_relation(seen, &relation, RelationKind::View, source)?;
        views.push(ViewMigration {
            old_key: Some(key),
            row: ViewRow {
                relation,
                definition_json: stored.definition_json,
            },
        });
    }
    if let Some(json) = catalog.get_metadata(LEGACY_VIEWS_METADATA_KEY)? {
        let legacy = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json)?;
        for (name, definition) in legacy {
            let relation =
                RelationIdentity::from_legacy_name(&name).map_err(StorageBackendError::Other)?;
            register_migration_relation(
                seen,
                &relation,
                RelationKind::View,
                format!("legacy metadata `{name}`"),
            )?;
            views.push(ViewMigration {
                old_key: None,
                row: ViewRow {
                    relation,
                    definition_json: serde_json::to_string(&definition)?,
                },
            });
        }
    }
    Ok(views)
}

fn collect_relation_migrations(
    catalog: &KeyValueCatalog,
) -> StorageBackendResult<RelationMigrations> {
    let mut seen = SeenRelations::new();
    let tables = collect_table_migrations(catalog.store.as_ref(), &mut seen)?;
    let sequences = collect_sequence_migrations(catalog, &mut seen)?;
    let foreign_tables = collect_foreign_migrations(catalog.store.as_ref(), &mut seen)?;
    let views = collect_view_migrations(catalog, &mut seen)?;
    Ok(RelationMigrations {
        seen,
        tables,
        sequences,
        foreign_tables,
        views,
    })
}

fn validate_relation_parents(
    store: &dyn KeyValueStore,
    seen: &SeenRelations,
) -> StorageBackendResult<()> {
    for (key, value) in store.scan_prefix(&key_with_tag(TAG_RELATION))? {
        let relation = decode_relation_key(&key)?;
        let stored = decode_value::<StoredRelation>(&value)?;
        match seen.get(&relation) {
            Some((kind, _)) if *kind == stored.kind => {}
            Some((kind, _)) => {
                return Err(StorageBackendError::Other(format!(
                    "catalog relation `{}` is {}, but its child is {}",
                    relation.qualified_name(),
                    stored.kind.as_str(),
                    kind.as_str()
                )));
            }
            None => {
                return Err(StorageBackendError::Other(format!(
                    "catalog relation `{}` has no {} child",
                    relation.qualified_name(),
                    stored.kind.as_str()
                )));
            }
        }
    }
    Ok(())
}

fn put_relation_parents(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    seen: &SeenRelations,
) -> StorageBackendResult<()> {
    for (relation, (kind, _)) in seen {
        if store
            .get(&single_str_key(TAG_SCHEMA, &relation.schema)?)?
            .is_none()
        {
            batch.put(
                &single_str_key(TAG_SCHEMA, &relation.schema)?,
                &string_value(&relation.schema),
            )?;
        }
        batch.put(
            &relation_key(TAG_RELATION, relation)?,
            &encode_value(&StoredRelation { kind: *kind })?,
        )?;
    }
    Ok(())
}

fn table_data_prefixes(table_name: &str) -> StorageBackendResult<[Vec<u8>; 8]> {
    Ok([
        document_key_prefix(table_name)?,
        posting_key_prefix(table_name)?,
        doc_length_key_prefix(table_name)?,
        field_stats_key_prefix(table_name)?,
        reverse_posting_key_prefix(table_name)?,
        vector_key_prefix(table_name)?,
        column_stats_prefix(table_name)?,
        table_field_analyzer_prefix(table_name)?,
    ])
}

fn apply_table_migration(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    table: TableMigration,
) -> StorageBackendResult<Option<(String, String)>> {
    let new_key = relation_key(TAG_TABLE, &table.schema.relation)?;
    batch.put(&new_key, &encode_value(&table.schema)?)?;
    if table.old_key != new_key {
        batch.delete(&table.old_key)?;
    }
    let canonical = table.schema.relation.qualified_name();
    if table.old_physical_name == canonical {
        return Ok(None);
    }
    for (old_prefix, new_prefix) in table_data_prefixes(&table.old_physical_name)?
        .into_iter()
        .zip(table_data_prefixes(&canonical)?)
    {
        ensure_prefix_absent(store, &new_prefix, &table.schema.relation)?;
        batch_rekey_prefix(store, batch, &old_prefix, &new_prefix)?;
    }
    Ok(Some((table.old_physical_name, canonical)))
}

fn put_sequence_migrations(
    batch: &mut dyn KeyValueBatch,
    sequences: Vec<SequenceMigration>,
) -> StorageBackendResult<()> {
    for sequence in sequences {
        let key = relation_key(TAG_SEQUENCE, &sequence.row.relation)?;
        batch.put(
            &key,
            &encode_value(&StoredSequence {
                start: sequence.row.start,
                increment: sequence.row.increment,
                current: sequence.row.current,
                called: sequence.row.called,
            })?,
        )?;
        if let Some(old_key) = sequence.old_key {
            if old_key != key {
                batch.delete(&old_key)?;
            }
        }
    }
    Ok(())
}

fn put_foreign_migrations(
    batch: &mut dyn KeyValueBatch,
    foreign_tables: Vec<ForeignMigration>,
) -> StorageBackendResult<()> {
    for foreign in foreign_tables {
        let key = relation_key(TAG_FOREIGN_TABLE, &foreign.relation)?;
        batch.put(&key, &foreign.value)?;
        if foreign.old_key != key {
            batch.delete(&foreign.old_key)?;
        }
    }
    Ok(())
}

fn put_view_migrations(
    batch: &mut dyn KeyValueBatch,
    views: Vec<ViewMigration>,
) -> StorageBackendResult<()> {
    for view in views {
        let key = relation_key(TAG_VIEW, &view.row.relation)?;
        batch.put(
            &key,
            &encode_value(&StoredView {
                definition_json: view.row.definition_json,
            })?,
        )?;
        if let Some(old_key) = view.old_key {
            if old_key != key {
                batch.delete(&old_key)?;
            }
        }
    }
    Ok(())
}

fn finish_relation_migration(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    table_renames: &std::collections::BTreeMap<String, String>,
) -> StorageBackendResult<()> {
    for (key, value) in store.scan_prefix(&key_with_tag(TAG_CATALOG_INDEX))? {
        let mut stored = decode_value::<StoredCatalogIndex>(&value)?;
        if let Some(canonical) = table_renames.get(&stored.table_name) {
            stored.table_name.clone_from(canonical);
            batch.put(&key, &encode_value(&stored)?)?;
        }
    }
    for metadata_key in [LEGACY_VIEWS_METADATA_KEY, LEGACY_SEQUENCES_METADATA_KEY] {
        batch.put(
            &single_str_key(TAG_METADATA, metadata_key)?,
            &string_value("{}"),
        )?;
    }
    Ok(())
}

fn apply_relation_migrations(
    catalog: &KeyValueCatalog,
    migrations: RelationMigrations,
) -> StorageBackendResult<()> {
    let mut batch = catalog.store.batch();
    put_relation_parents(catalog.store.as_ref(), batch.as_mut(), &migrations.seen)?;
    let mut table_renames = std::collections::BTreeMap::new();
    for table in migrations.tables {
        if let Some((old, new)) =
            apply_table_migration(catalog.store.as_ref(), batch.as_mut(), table)?
        {
            table_renames.insert(old, new);
        }
    }
    put_sequence_migrations(batch.as_mut(), migrations.sequences)?;
    put_foreign_migrations(batch.as_mut(), migrations.foreign_tables)?;
    put_view_migrations(batch.as_mut(), migrations.views)?;
    finish_relation_migration(catalog.store.as_ref(), batch.as_mut(), &table_renames)?;
    batch.commit()
}

fn graph_membership_prefix() -> Vec<u8> {
    key_with_tag(TAG_GRAPH_MEMBERSHIP)
}

fn graph_membership_graph_prefix(graph_name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = graph_membership_prefix();
    push_str(&mut key, graph_name)?;
    Ok(key)
}

fn graph_membership_key(
    entity_type: &str,
    entity_id: u64,
    graph_name: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = graph_membership_graph_prefix(graph_name)?;
    push_str(&mut key, entity_type)?;
    push_u64(&mut key, entity_id);
    Ok(key)
}

fn table_field_analyzer_prefix(table_name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(TAG_TABLE_FIELD_ANALYZER);
    push_str(&mut key, table_name)?;
    Ok(key)
}

fn table_field_analyzer_key(
    table_name: &str,
    field: &str,
    phase: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = table_field_analyzer_field_prefix(table_name, field)?;
    push_str(&mut key, phase)?;
    Ok(key)
}

fn table_field_analyzer_field_prefix(
    table_name: &str,
    field: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = table_field_analyzer_prefix(table_name)?;
    push_str(&mut key, field)?;
    Ok(key)
}

fn column_stats_prefix(table_name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = key_with_tag(TAG_COLUMN_STATS);
    push_str(&mut key, table_name)?;
    Ok(key)
}

fn column_stats_key(table_name: &str, column_name: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = column_stats_prefix(table_name)?;
    push_str(&mut key, column_name)?;
    Ok(key)
}

fn vertex_key(vertex_id: u64) -> Vec<u8> {
    let mut key = key_with_tag(TAG_VERTEX);
    push_u64(&mut key, vertex_id);
    key
}

fn edge_key(edge_id: u64) -> Vec<u8> {
    let mut key = key_with_tag(TAG_EDGE);
    push_u64(&mut key, edge_id);
    key
}

/// Catalog facade implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueCatalog {
    store: Arc<dyn KeyValueStore>,
    sequence_lock: Arc<Mutex<()>>,
}

fn batch_rekey_prefix(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    old_prefix: &[u8],
    new_prefix: &[u8],
) -> StorageBackendResult<()> {
    for (key, value) in store.scan_prefix(old_prefix)? {
        let mut new_key = new_prefix.to_vec();
        new_key.extend_from_slice(&key[old_prefix.len()..]);
        batch.put(&new_key, &value)?;
        batch.delete(&key)?;
    }
    Ok(())
}

fn batch_rekey_prefix_or_keep_existing(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    old_prefix: &[u8],
    new_prefix: &[u8],
) -> StorageBackendResult<()> {
    for (key, value) in store.scan_prefix(old_prefix)? {
        let mut new_key = new_prefix.to_vec();
        new_key.extend_from_slice(&key[old_prefix.len()..]);
        if store.get(&new_key)?.is_none() {
            batch.put(&new_key, &value)?;
        }
        batch.delete(&key)?;
    }
    Ok(())
}

fn batch_put_or_keep_existing(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    key: &[u8],
    value: &[u8],
) -> StorageBackendResult<()> {
    if store.get(key)?.is_none() {
        batch.put(key, value)?;
    }
    Ok(())
}

fn catalog_index_references_column(
    row: &CatalogIndexRow,
    column_name: &str,
) -> StorageBackendResult<bool> {
    let columns = serde_json::from_str::<Vec<String>>(&row.columns_json)?;
    Ok(columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case(column_name)))
}

fn catalog_index_rename_column(
    row: &CatalogIndexRow,
    from: &str,
    to: &str,
) -> StorageBackendResult<Option<String>> {
    let mut columns = serde_json::from_str::<Vec<String>>(&row.columns_json)?;
    let mut changed = false;
    for column in &mut columns {
        if column.eq_ignore_ascii_case(from) {
            *column = to.to_string();
            changed = true;
        }
    }
    if changed {
        Ok(Some(serde_json::to_string(&columns)?))
    } else {
        Ok(None)
    }
}

impl KeyValueCatalog {
    pub fn new(store: Arc<dyn KeyValueStore>) -> Self {
        Self {
            store,
            sequence_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn store(&self) -> Arc<dyn KeyValueStore> {
        Arc::clone(&self.store)
    }

    fn ensure_schema_exists(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        if self
            .store
            .get(&single_str_key(TAG_SCHEMA, &relation.schema)?)?
            .is_none()
        {
            return Err(StorageBackendError::Other(format!(
                "schema `{}` does not exist for relation `{}`",
                relation.schema,
                relation.qualified_name()
            )));
        }
        Ok(())
    }

    fn claim_relation(
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> StorageBackendResult<()> {
        self.ensure_schema_exists(relation)?;
        let key = relation_key(TAG_RELATION, relation)?;
        if let Some(value) = self.store.get(&key)? {
            let existing = decode_value::<StoredRelation>(&value)?.kind;
            if existing != kind {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists as {}",
                    relation.qualified_name(),
                    existing.as_str()
                )));
            }
        } else {
            batch.put(&key, &encode_value(&StoredRelation { kind })?)?;
        }
        Ok(())
    }

    fn release_relation(
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> StorageBackendResult<()> {
        let key = relation_key(TAG_RELATION, relation)?;
        if let Some(value) = self.store.get(&key)? {
            let existing = decode_value::<StoredRelation>(&value)?.kind;
            if existing != kind {
                return Err(StorageBackendError::Other(format!(
                    "catalog relation `{}` is {}, not {}",
                    relation.qualified_name(),
                    existing.as_str(),
                    kind.as_str()
                )));
            }
            batch.delete(&key)?;
        }
        Ok(())
    }
}

impl CatalogFacade for KeyValueCatalog {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_METADATA, key)?, &string_value(value))
    }

    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_METADATA, key)?)?
            .map(decode_string)
            .transpose()
    }

    fn migrate_relation_namespace(&self) -> StorageBackendResult<()> {
        let migrations = collect_relation_migrations(self)?;
        validate_relation_parents(self.store.as_ref(), &migrations.seen)?;
        apply_relation_migrations(self, migrations)
    }

    fn save_schema(&self, name: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_SCHEMA, name)?, &string_value(name))
    }

    fn drop_schema(&self, name: &str) -> StorageBackendResult<()> {
        for (key, _) in self.store.scan_prefix(&key_with_tag(TAG_RELATION))? {
            if decode_relation_key(&key)?.schema == name {
                return Err(StorageBackendError::Other(format!(
                    "schema `{name}` still owns catalog relations"
                )));
            }
        }
        self.store.delete(&single_str_key(TAG_SCHEMA, name)?)
    }

    fn load_schemas(&self) -> StorageBackendResult<Vec<String>> {
        let mut rows = Vec::new();
        for (key, _) in self.store.scan_prefix(&key_with_tag(TAG_SCHEMA))? {
            let mut offset = 1;
            rows.push(read_str(&key, &mut offset)?);
        }
        rows.sort();
        Ok(rows)
    }

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), &schema.relation, RelationKind::Table)?;
        batch.put(
            &relation_key(TAG_TABLE, &schema.relation)?,
            &encode_value(schema)?,
        )?;
        batch.commit()
    }

    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>> {
        let mut rows = self
            .store
            .scan_prefix(&key_with_tag(TAG_TABLE))?
            .into_iter()
            .map(|(key, value)| {
                let relation = decode_relation_key(&key)?;
                let schema = decode_value::<TableSchema>(&value)?;
                if schema.relation != relation {
                    return Err(StorageBackendError::Other(format!(
                        "table catalog key `{}` disagrees with stored relation `{}`",
                        relation.qualified_name(),
                        schema.relation.qualified_name()
                    )));
                }
                Ok(schema)
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|a, b| a.relation.cmp(&b.relation));
        Ok(rows)
    }

    fn drop_table(&self, name: &str) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let mut batch = self.store.batch();
        batch.delete(&relation_key(TAG_TABLE, &relation)?)?;
        self.release_relation(batch.as_mut(), &relation, RelationKind::Table)?;
        batch.commit()
    }

    fn drop_table_and_data(&self, name: &str) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let storage_names = relation.canonical_and_legacy_public_names();
        let mut batch = self.store.batch();
        batch.delete(&relation_key(TAG_TABLE, &relation)?)?;
        self.release_relation(batch.as_mut(), &relation, RelationKind::Table)?;
        for storage_name in &storage_names {
            batch.delete_prefix(&document_key_prefix(storage_name)?)?;
            batch.delete_prefix(&posting_key_prefix(storage_name)?)?;
            batch.delete_prefix(&doc_length_key_prefix(storage_name)?)?;
            batch.delete_prefix(&field_stats_key_prefix(storage_name)?)?;
            batch.delete_prefix(&reverse_posting_key_prefix(storage_name)?)?;
            batch.delete_prefix(&vector_key_prefix(storage_name)?)?;
            batch.delete_prefix(&column_stats_prefix(storage_name)?)?;
            batch.delete_prefix(&table_field_analyzer_prefix(storage_name)?)?;
        }
        for row in self.load_catalog_indexes()? {
            if storage_names.contains(&row.table_name) {
                batch.delete(&single_str_key(TAG_CATALOG_INDEX, &row.name)?)?;
            }
        }
        batch.commit()
    }

    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let mut batch = self.store.batch();
        for storage_name in relation.canonical_and_legacy_public_names() {
            batch.delete_prefix(&document_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&posting_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&doc_length_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&field_stats_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&reverse_posting_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&vector_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&column_stats_prefix(&storage_name)?)?;
        }
        batch.commit()
    }

    fn rename_table_data(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        let from_relation =
            RelationIdentity::from_legacy_name(from).map_err(StorageBackendError::Other)?;
        let to_relation =
            RelationIdentity::from_legacy_name(to).map_err(StorageBackendError::Other)?;
        if from_relation == to_relation {
            return Ok(());
        }
        self.ensure_schema_exists(&to_relation)?;
        let from_key = relation_key(TAG_TABLE, &from_relation)?;
        let to_key = relation_key(TAG_TABLE, &to_relation)?;
        if self.store.get(&to_key)?.is_some()
            || self
                .store
                .get(&relation_key(TAG_RELATION, &to_relation)?)?
                .is_some()
        {
            return Err(StorageBackendError::Other(format!(
                "relation `{}` already exists",
                to_relation.qualified_name()
            )));
        }
        let value = self
            .store
            .get(&from_key)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{from}` does not exist")))?;
        let mut batch = self.store.batch();
        let mut schema = decode_value::<TableSchema>(&value)?;
        schema.relation = to_relation.clone();
        batch.put(&to_key, &encode_value(&schema)?)?;
        batch.delete(&from_key)?;
        self.release_relation(batch.as_mut(), &from_relation, RelationKind::Table)?;
        batch.put(
            &relation_key(TAG_RELATION, &to_relation)?,
            &encode_value(&StoredRelation {
                kind: RelationKind::Table,
            })?,
        )?;
        for (old_prefix, new_prefix) in [
            (document_key_prefix(from)?, document_key_prefix(to)?),
            (posting_key_prefix(from)?, posting_key_prefix(to)?),
            (doc_length_key_prefix(from)?, doc_length_key_prefix(to)?),
            (field_stats_key_prefix(from)?, field_stats_key_prefix(to)?),
            (
                reverse_posting_key_prefix(from)?,
                reverse_posting_key_prefix(to)?,
            ),
            (vector_key_prefix(from)?, vector_key_prefix(to)?),
            (column_stats_prefix(from)?, column_stats_prefix(to)?),
            (
                table_field_analyzer_prefix(from)?,
                table_field_analyzer_prefix(to)?,
            ),
        ] {
            batch_rekey_prefix(
                self.store.as_ref(),
                batch.as_mut(),
                &old_prefix,
                &new_prefix,
            )?;
        }
        for row in self.load_catalog_indexes()? {
            if row.table_name == from {
                batch.put(
                    &single_str_key(TAG_CATALOG_INDEX, &row.name)?,
                    &encode_value(&StoredCatalogIndex {
                        index_type: row.index_type,
                        table_name: to.to_string(),
                        columns_json: row.columns_json,
                        parameters_json: row.parameters_json,
                    })?,
                )?;
            }
        }
        batch.commit()
    }

    fn drop_column_data(&self, table_name: &str, column_name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for (key, value) in self.store.scan_prefix(&document_key_prefix(table_name)?)? {
            let mut document = decode_document_value(&value)?;
            if document.remove(column_name).is_some() {
                batch.put(&key, &encode_document_value(&document)?)?;
            }
        }
        batch.delete_prefix(&posting_field_prefix(table_name, column_name)?)?;
        batch.delete_prefix(&field_stats_key(table_name, column_name)?)?;
        batch.delete_prefix(&vector_field_prefix(table_name, column_name)?)?;
        batch.delete_prefix(&table_field_analyzer_field_prefix(table_name, column_name)?)?;
        batch.delete(&column_stats_key(table_name, column_name)?)?;
        for (key, _) in self
            .store
            .scan_prefix(&doc_length_key_prefix(table_name)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(column_name) {
                batch.delete(&key)?;
            }
        }
        for (key, _) in self
            .store
            .scan_prefix(&reverse_posting_key_prefix(table_name)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(column_name) {
                batch.delete(&key)?;
            }
        }
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name && catalog_index_references_column(&row, column_name)? {
                batch.delete(&single_str_key(TAG_CATALOG_INDEX, &row.name)?)?;
            }
        }
        batch.commit()
    }

    fn rename_column_data(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for (key, value) in self.store.scan_prefix(&document_key_prefix(table_name)?)? {
            let mut document = decode_document_value(&value)?;
            if let Some(value) = document.remove(from) {
                document.insert(to.to_string(), value);
                batch.put(&key, &encode_document_value(&document)?)?;
            }
        }
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &posting_field_prefix(table_name, from)?,
            &posting_field_prefix(table_name, to)?,
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &field_stats_key(table_name, from)?,
            &field_stats_key(table_name, to)?,
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &vector_field_prefix(table_name, from)?,
            &vector_field_prefix(table_name, to)?,
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &table_field_analyzer_field_prefix(table_name, from)?,
            &table_field_analyzer_field_prefix(table_name, to)?,
        )?;
        if let Some(value) = self.store.get(&column_stats_key(table_name, from)?)? {
            batch_put_or_keep_existing(
                self.store.as_ref(),
                batch.as_mut(),
                &column_stats_key(table_name, to)?,
                &value,
            )?;
            batch.delete(&column_stats_key(table_name, from)?)?;
        }
        for (key, value) in self
            .store
            .scan_prefix(&doc_length_key_prefix(table_name)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(from) {
                batch_put_or_keep_existing(
                    self.store.as_ref(),
                    batch.as_mut(),
                    &doc_length_key(table_name, doc_id, to)?,
                    &value,
                )?;
                batch.delete(&key)?;
            }
        }
        for (key, value) in self
            .store
            .scan_prefix(&reverse_posting_key_prefix(table_name)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(from) {
                batch_put_or_keep_existing(
                    self.store.as_ref(),
                    batch.as_mut(),
                    &reverse_posting_key(table_name, doc_id, to, &term)?,
                    &value,
                )?;
                batch.delete(&key)?;
            }
        }
        for row in self.load_catalog_indexes()? {
            if row.table_name != table_name {
                continue;
            }
            if let Some(columns_json) = catalog_index_rename_column(&row, from, to)? {
                batch.put(
                    &single_str_key(TAG_CATALOG_INDEX, &row.name)?,
                    &encode_value(&StoredCatalogIndex {
                        index_type: row.index_type,
                        table_name: row.table_name,
                        columns_json,
                        parameters_json: row.parameters_json,
                    })?,
                )?;
            }
        }
        batch.commit()
    }

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_MODEL, name)?, &string_value(json))
    }

    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_MODEL)
    }

    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_MODEL, name)?)?
            .map(decode_string)
            .transpose()
    }

    fn drop_model(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_MODEL, name)?)
    }

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_SCORING_PARAMS, name)?,
            &string_value(params_json),
        )
    }

    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_SCORING_PARAMS, name)?)?
            .map(decode_string)
            .transpose()
    }

    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_SCORING_PARAMS)
    }

    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()> {
        self.store
            .delete(&single_str_key(TAG_SCORING_PARAMS, name)?)
    }

    fn create_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool> {
        let _guard = self.sequence_lock.lock();
        self.ensure_schema_exists(&sequence.relation)?;
        let key = relation_key(TAG_SEQUENCE, &sequence.relation)?;
        if self.store.get(&key)?.is_some() {
            return Ok(false);
        }
        let relation_key = relation_key(TAG_RELATION, &sequence.relation)?;
        if let Some(value) = self.store.get(&relation_key)? {
            let existing = decode_value::<StoredRelation>(&value)?.kind;
            if existing != RelationKind::Sequence {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists as {}",
                    sequence.relation.qualified_name(),
                    existing.as_str()
                )));
            }
        }
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), &sequence.relation, RelationKind::Sequence)?;
        batch.put(
            &key,
            &encode_value(&StoredSequence {
                start: sequence.start,
                increment: sequence.increment,
                current: sequence.current,
                called: sequence.called,
            })?,
        )?;
        batch.commit()?;
        Ok(true)
    }

    fn replace_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool> {
        let _guard = self.sequence_lock.lock();
        let key = relation_key(TAG_SEQUENCE, &sequence.relation)?;
        if self.store.get(&key)?.is_none() {
            return Ok(false);
        }
        self.store.put(
            &key,
            &encode_value(&StoredSequence {
                start: sequence.start,
                increment: sequence.increment,
                current: sequence.current,
                called: sequence.called,
            })?,
        )?;
        Ok(true)
    }

    fn drop_sequence_row(&self, name: &str) -> StorageBackendResult<bool> {
        let _guard = self.sequence_lock.lock();
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        let existed = self.store.get(&key)?.is_some();
        if existed {
            let mut batch = self.store.batch();
            batch.delete(&key)?;
            self.release_relation(batch.as_mut(), &relation, RelationKind::Sequence)?;
            batch.commit()?;
        }
        Ok(existed)
    }

    fn load_sequence_rows(&self) -> StorageBackendResult<Vec<SequenceRow>> {
        let mut rows = self
            .store
            .scan_prefix(&key_with_tag(TAG_SEQUENCE))?
            .into_iter()
            .map(|(key, value)| {
                let relation = decode_relation_key(&key)?;
                let stored: StoredSequence = decode_value(&value)?;
                Ok(SequenceRow {
                    relation,
                    start: stored.start,
                    increment: stored.increment,
                    current: stored.current,
                    called: stored.called,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(rows)
    }

    fn next_sequence_value(&self, name: &str) -> StorageBackendResult<Option<i64>> {
        let _guard = self.sequence_lock.lock();
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        let Some(value) = self.store.get(&key)? else {
            return Ok(None);
        };
        let mut stored: StoredSequence = decode_value(&value)?;
        if stored.called {
            stored.current = stored
                .current
                .checked_add(stored.increment)
                .ok_or_else(|| {
                    crate::StorageBackendError::Other(format!("sequence `{name}` overflow"))
                })?;
        } else {
            stored.called = true;
        }
        let current = stored.current;
        self.store.put(&key, &encode_value(&stored)?)?;
        Ok(Some(current))
    }

    fn set_sequence_value(&self, name: &str, value: i64) -> StorageBackendResult<Option<i64>> {
        let _guard = self.sequence_lock.lock();
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        let Some(encoded) = self.store.get(&key)? else {
            return Ok(None);
        };
        let mut stored: StoredSequence = decode_value(&encoded)?;
        stored.current = value;
        stored.called = true;
        self.store.put(&key, &encode_value(&stored)?)?;
        Ok(Some(value))
    }

    fn save_view(&self, view: &ViewRow) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), &view.relation, RelationKind::View)?;
        batch.put(
            &relation_key(TAG_VIEW, &view.relation)?,
            &encode_value(&StoredView {
                definition_json: view.definition_json.clone(),
            })?,
        )?;
        batch.commit()
    }

    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<bool> {
        let key = relation_key(TAG_VIEW, relation)?;
        if self.store.get(&key)?.is_none() {
            return Ok(false);
        }
        let mut batch = self.store.batch();
        batch.delete(&key)?;
        self.release_relation(batch.as_mut(), relation, RelationKind::View)?;
        batch.commit()?;
        Ok(true)
    }

    fn load_views(&self) -> StorageBackendResult<Vec<ViewRow>> {
        let mut rows = self
            .store
            .scan_prefix(&key_with_tag(TAG_VIEW))?
            .into_iter()
            .map(|(key, value)| {
                let relation = decode_relation_key(&key)?;
                let stored = decode_value::<StoredView>(&value)?;
                Ok(ViewRow {
                    relation,
                    definition_json: stored.definition_json,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(rows)
    }

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        self.store.put(&single_str_key(TAG_NAMED_GRAPH, name)?, &[])
    }

    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&single_str_key(TAG_NAMED_GRAPH, name)?)?;
        batch.delete_prefix(&graph_membership_graph_prefix(name)?)?;
        batch.commit()
    }

    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>> {
        load_single_keys(self.store.as_ref(), TAG_NAMED_GRAPH)
    }

    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let key = vertex_key(vertex_id);
        self.store.put(
            &key,
            &encode_value(&StoredVertex {
                label: label.to_string(),
                properties_json: properties_json.to_string(),
            })?,
        )
    }

    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()> {
        let key = vertex_key(vertex_id);
        self.store.delete(&key)
    }

    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_VERTEX))? {
            let mut offset = 1;
            let vertex_id = read_u64(&key, &mut offset)?;
            let stored: StoredVertex = decode_value(&value)?;
            rows.push((vertex_id, stored.label, stored.properties_json));
        }
        Ok(rows)
    }

    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let key = edge_key(edge_id);
        self.store.put(
            &key,
            &encode_value(&StoredEdge {
                source_id,
                target_id,
                label: label.to_string(),
                properties_json: properties_json.to_string(),
            })?,
        )
    }

    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()> {
        let key = edge_key(edge_id);
        self.store.delete(&key)
    }

    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_EDGE))? {
            let mut offset = 1;
            let edge_id = read_u64(&key, &mut offset)?;
            let stored: StoredEdge = decode_value(&value)?;
            rows.push(EdgeRow {
                edge_id,
                source_id: stored.source_id,
                target_id: stored.target_id,
                label: stored.label,
                properties_json: stored.properties_json,
            });
        }
        Ok(rows)
    }

    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &graph_membership_key(entity_type, entity_id, graph_name)?,
            &[],
        )
    }

    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store
            .delete(&graph_membership_key(entity_type, entity_id, graph_name)?)
    }

    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&graph_membership_graph_prefix(graph_name)?)?;
        Ok(())
    }

    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>> {
        let mut rows = Vec::new();
        for (key, _) in self.store.scan_prefix(&graph_membership_prefix())? {
            let mut offset = 1;
            let graph_name = read_str(&key, &mut offset)?;
            let entity_type = read_str(&key, &mut offset)?;
            let entity_id = read_u64(&key, &mut offset)?;
            rows.push((entity_type, entity_id, graph_name));
        }
        Ok(rows)
    }

    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()> {
        let memberships = self.load_graph_memberships()?;
        let vertex_ids = memberships
            .iter()
            .filter_map(|(ty, id, _)| (ty == "vertex").then_some(*id))
            .collect::<BTreeSet<_>>();
        let edge_ids = memberships
            .iter()
            .filter_map(|(ty, id, _)| (ty == "edge").then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut batch = self.store.batch();
        for (id, _, _) in self.load_vertices()? {
            if !vertex_ids.contains(&id) {
                batch.delete(&vertex_key(id))?;
            }
        }
        for edge in self.load_edges()? {
            if !edge_ids.contains(&edge.edge_id) {
                batch.delete(&edge_key(edge.edge_id))?;
            }
        }
        batch.commit()
    }

    fn replace_named_graph(
        &self,
        graph_name: &str,
        snapshot: &GraphSnapshot,
    ) -> StorageBackendResult<()> {
        let memberships = self.load_graph_memberships()?;
        let mut surviving_vertices = memberships
            .iter()
            .filter_map(|(ty, id, graph)| (graph != graph_name && ty == "vertex").then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut surviving_edges = memberships
            .iter()
            .filter_map(|(ty, id, graph)| (graph != graph_name && ty == "edge").then_some(*id))
            .collect::<BTreeSet<_>>();
        surviving_vertices.extend(snapshot.vertices.iter().map(|row| row.vertex_id));
        surviving_edges.extend(snapshot.edges.iter().map(|row| row.edge_id));
        let mut batch = self.store.batch();
        batch.put(&single_str_key(TAG_NAMED_GRAPH, graph_name)?, &[])?;
        batch.delete_prefix(&graph_membership_graph_prefix(graph_name)?)?;
        for (key, _) in self.store.scan_prefix(&key_with_tag(TAG_PATH_INDEX))? {
            let mut offset = 1;
            let key_name = read_str(&key, &mut offset)?;
            if key_name.starts_with(&format!("{graph_name}::")) {
                batch.delete(&key)?;
            }
        }
        for vertex in &snapshot.vertices {
            batch.put(
                &vertex_key(vertex.vertex_id),
                &encode_value(&StoredVertex {
                    label: vertex.label.clone(),
                    properties_json: vertex.properties_json.clone(),
                })?,
            )?;
            batch.put(
                &graph_membership_key("vertex", vertex.vertex_id, graph_name)?,
                &[],
            )?;
        }
        for edge in &snapshot.edges {
            batch.put(
                &edge_key(edge.edge_id),
                &encode_value(&StoredEdge {
                    source_id: edge.source_id,
                    target_id: edge.target_id,
                    label: edge.label.clone(),
                    properties_json: edge.properties_json.clone(),
                })?,
            )?;
            batch.put(
                &graph_membership_key("edge", edge.edge_id, graph_name)?,
                &[],
            )?;
        }
        batch.put(
            &single_str_key(TAG_METADATA, &format!("graph_label_registry::{graph_name}"))?,
            &string_value(&snapshot.label_registry_json),
        )?;
        for (id, _, _) in self.load_vertices()? {
            if !surviving_vertices.contains(&id) {
                batch.delete(&vertex_key(id))?;
            }
        }
        for edge in self.load_edges()? {
            if !surviving_edges.contains(&edge.edge_id) {
                batch.delete(&edge_key(edge.edge_id))?;
            }
        }
        batch.commit()
    }

    fn drop_named_graph_data(&self, graph_name: &str) -> StorageBackendResult<()> {
        let memberships = self.load_graph_memberships()?;
        let surviving_vertices = memberships
            .iter()
            .filter_map(|(ty, id, graph)| (graph != graph_name && ty == "vertex").then_some(*id))
            .collect::<BTreeSet<_>>();
        let surviving_edges = memberships
            .iter()
            .filter_map(|(ty, id, graph)| (graph != graph_name && ty == "edge").then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut batch = self.store.batch();
        batch.delete(&single_str_key(TAG_NAMED_GRAPH, graph_name)?)?;
        batch.delete_prefix(&graph_membership_graph_prefix(graph_name)?)?;
        batch.delete(&single_str_key(
            TAG_METADATA,
            &format!("graph_label_registry::{graph_name}"),
        )?)?;
        for (key, _) in self.store.scan_prefix(&key_with_tag(TAG_PATH_INDEX))? {
            let mut offset = 1;
            let key_name = read_str(&key, &mut offset)?;
            if key_name.starts_with(&format!("{graph_name}::")) {
                batch.delete(&key)?;
            }
        }
        for (id, _, _) in self.load_vertices()? {
            if !surviving_vertices.contains(&id) {
                batch.delete(&vertex_key(id))?;
            }
        }
        for edge in self.load_edges()? {
            if !surviving_edges.contains(&edge.edge_id) {
                batch.delete(&edge_key(edge.edge_id))?;
            }
        }
        batch.commit()
    }

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_ANALYZER, name)?,
            &string_value(config_json),
        )
    }

    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_ANALYZER, name)?)
    }

    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_ANALYZER)
    }

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &table_field_analyzer_key(table_name, field, phase)?,
            &string_value(analyzer_name),
        )
    }

    fn replace_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&table_field_analyzer_field_prefix(table_name, field)?)?;
        batch.put(
            &table_field_analyzer_key(table_name, field, phase)?,
            &string_value(analyzer_name),
        )?;
        batch.commit()
    }

    fn drop_table_field_analyzer_field(
        &self,
        table_name: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&table_field_analyzer_field_prefix(table_name, field)?)?;
        Ok(())
    }

    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&table_field_analyzer_prefix(table_name)?)?;
        Ok(())
    }

    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self
            .store
            .scan_prefix(&key_with_tag(TAG_TABLE_FIELD_ANALYZER))?
        {
            let mut offset = 1;
            let table = read_str(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let phase = read_str(&key, &mut offset)?;
            rows.push((table, field, phase, decode_string(value)?));
        }
        rows.sort();
        Ok(rows)
    }

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_FOREIGN_SERVER, name)?,
            &encode_value(&StoredForeignServer {
                fdw_type: fdw_type.to_string(),
                options_json: options_json.to_string(),
            })?,
        )
    }

    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()> {
        self.store
            .delete(&single_str_key(TAG_FOREIGN_SERVER, name)?)
    }

    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_FOREIGN_SERVER))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredForeignServer = decode_value(&value)?;
            rows.push((name, stored.fdw_type, stored.options_json));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows)
    }

    fn save_foreign_table(
        &self,
        relation: &RelationIdentity,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), relation, RelationKind::ForeignTable)?;
        batch.put(
            &relation_key(TAG_FOREIGN_TABLE, relation)?,
            &encode_value(&StoredForeignTable {
                server_name: server_name.to_string(),
                columns_json: columns_json.to_string(),
                options_json: options_json.to_string(),
            })?,
        )?;
        batch.commit()
    }

    fn drop_foreign_table(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&relation_key(TAG_FOREIGN_TABLE, relation)?)?;
        self.release_relation(batch.as_mut(), relation, RelationKind::ForeignTable)?;
        batch.commit()
    }

    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_FOREIGN_TABLE))? {
            let relation = decode_relation_key(&key)?;
            let stored: StoredForeignTable = decode_value(&value)?;
            rows.push(ForeignTableRow {
                relation,
                server_name: stored.server_name,
                columns_json: stored.columns_json,
                options_json: stored.options_json,
            });
        }
        rows.sort_by(|a, b| a.relation.cmp(&b.relation));
        Ok(rows)
    }

    fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_CATALOG_INDEX, name)?,
            &encode_value(&StoredCatalogIndex {
                index_type: index_type.to_string(),
                table_name: table_name.to_string(),
                columns_json: columns_json.to_string(),
                parameters_json: parameters_json.to_string(),
            })?,
        )
    }

    fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_CATALOG_INDEX, name)?)
    }

    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name {
                batch.delete(&single_str_key(TAG_CATALOG_INDEX, &row.name)?)?;
            }
        }
        batch.commit()
    }

    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_CATALOG_INDEX))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredCatalogIndex = decode_value(&value)?;
            rows.push(CatalogIndexRow {
                name,
                index_type: stored.index_type,
                table_name: stored.table_name,
                columns_json: stored.columns_json,
                parameters_json: stored.parameters_json,
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_PATH_INDEX, graph_name)?,
            &string_value(label_sequences_json),
        )
    }

    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete(&single_str_key(TAG_PATH_INDEX, graph_name)?)
    }

    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_PATH_INDEX)
    }

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()> {
        self.store.put(
            &column_stats_key(stats.table_name, stats.column_name)?,
            &encode_value(&StoredColumnStats {
                distinct_count: stats.distinct_count,
                null_count: stats.null_count,
                min_value: stats.min_value.map(str::to_string),
                max_value: stats.max_value.map(str::to_string),
                row_count: stats.row_count,
                histogram_json: stats.histogram_json.to_string(),
                mcv_values_json: stats.mcv_values_json.to_string(),
                mcv_frequencies_json: stats.mcv_frequencies_json.to_string(),
            })?,
        )
    }

    fn replace_column_stats(
        &self,
        table_name: &str,
        stats: &[ColumnStatsInput<'_>],
    ) -> StorageBackendResult<()> {
        let mut encoded = Vec::with_capacity(stats.len());
        for row in stats {
            if row.table_name != table_name {
                return Err(StorageBackendError::Other(format!(
                    "column stats row for table `{}` cannot be stored in snapshot `{table_name}`",
                    row.table_name
                )));
            }
            encoded.push((
                column_stats_key(row.table_name, row.column_name)?,
                encode_value(&StoredColumnStats {
                    distinct_count: row.distinct_count,
                    null_count: row.null_count,
                    min_value: row.min_value.map(str::to_string),
                    max_value: row.max_value.map(str::to_string),
                    row_count: row.row_count,
                    histogram_json: row.histogram_json.to_string(),
                    mcv_values_json: row.mcv_values_json.to_string(),
                    mcv_frequencies_json: row.mcv_frequencies_json.to_string(),
                })?,
            ));
        }
        let mut batch = self.store.batch();
        batch.delete_prefix(&column_stats_prefix(table_name)?)?;
        for (key, value) in encoded {
            batch.put(&key, &value)?;
        }
        batch.commit()
    }

    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&column_stats_prefix(table_name)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let column_name = read_str(&key, &mut offset)?;
            let stored: StoredColumnStats = decode_value(&value)?;
            rows.push(ColumnStatsRow {
                column_name,
                distinct_count: stored.distinct_count,
                null_count: stored.null_count,
                min_value: stored.min_value,
                max_value: stored.max_value,
                row_count: stored.row_count,
                histogram_json: stored.histogram_json,
                mcv_values_json: stored.mcv_values_json,
                mcv_frequencies_json: stored.mcv_frequencies_json,
            });
        }
        rows.sort_by(|a, b| a.column_name.cmp(&b.column_name));
        Ok(rows)
    }

    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&column_stats_prefix(table_name)?)?;
        Ok(())
    }
}

fn load_single_keys(store: &dyn KeyValueStore, tag: u8) -> StorageBackendResult<Vec<String>> {
    let mut rows = store
        .scan_prefix(&key_with_tag(tag))?
        .into_iter()
        .map(|(key, _)| {
            let mut offset = 1;
            read_str(&key, &mut offset)
        })
        .collect::<StorageBackendResult<Vec<_>>>()?;
    rows.sort();
    Ok(rows)
}

fn load_single_string_rows(
    store: &dyn KeyValueStore,
    tag: u8,
) -> StorageBackendResult<Vec<(String, String)>> {
    let mut rows = store
        .scan_prefix(&key_with_tag(tag))?
        .into_iter()
        .map(|(key, value)| {
            let mut offset = 1;
            Ok((read_str(&key, &mut offset)?, decode_string(value)?))
        })
        .collect::<StorageBackendResult<Vec<_>>>()?;
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_store::Document;
    use crate::key_value::MemoryKeyValueStore;

    fn legacy_table_value(name: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "name": name,
            "analyzer_json": "{}",
            "fts_fields": [],
            "vector_fields": [],
            "columns_json": "[]",
            "constraints_json": ""
        }))
        .unwrap()
    }

    #[test]
    fn relation_namespace_migration_is_one_batch_and_moves_public_data() {
        let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
        let catalog = KeyValueCatalog::new(Arc::clone(&store));
        store
            .put(
                &single_str_key(TAG_TABLE, "docs").unwrap(),
                &legacy_table_value("docs"),
            )
            .unwrap();
        store
            .put(
                &single_str_key(TAG_SEQUENCE, "seq").unwrap(),
                br#"{"start":1,"increment":1,"current":0}"#,
            )
            .unwrap();
        store
            .put(
                &document_key_prefix("docs").unwrap(),
                &encode_value(&Document::new()).unwrap(),
            )
            .unwrap();
        catalog
            .set_metadata(LEGACY_VIEWS_METADATA_KEY, r#"{"report":{"plan":1}}"#)
            .unwrap();

        catalog.migrate_relation_namespace().unwrap();

        assert_eq!(
            catalog.load_tables().unwrap()[0].relation,
            RelationIdentity::new("public", "docs")
        );
        assert_eq!(
            catalog.load_sequence_rows().unwrap()[0].relation,
            RelationIdentity::new("public", "seq")
        );
        assert_eq!(catalog.next_sequence_value("public.seq").unwrap(), Some(1));
        assert_eq!(
            catalog.load_views().unwrap()[0].relation,
            RelationIdentity::new("public", "report")
        );
        assert!(catalog
            .load_schemas()
            .unwrap()
            .contains(&"public".to_string()));
        assert!(store
            .get(&document_key_prefix("public.docs").unwrap())
            .unwrap()
            .is_some());
        assert!(store
            .get(&document_key_prefix("docs").unwrap())
            .unwrap()
            .is_none());
    }

    #[test]
    fn relation_namespace_migration_rejects_alias_and_cross_kind_collisions() {
        for cross_kind in [false, true] {
            let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
            let catalog = KeyValueCatalog::new(Arc::clone(&store));
            store
                .put(
                    &single_str_key(TAG_TABLE, "docs").unwrap(),
                    &legacy_table_value("docs"),
                )
                .unwrap();
            if cross_kind {
                store
                    .put(
                        &single_str_key(TAG_SEQUENCE, "public.docs").unwrap(),
                        &encode_value(&StoredSequence {
                            start: 1,
                            increment: 1,
                            current: 0,
                            called: true,
                        })
                        .unwrap(),
                    )
                    .unwrap();
            } else {
                store
                    .put(
                        &single_str_key(TAG_TABLE, "public.docs").unwrap(),
                        &legacy_table_value("public.docs"),
                    )
                    .unwrap();
            }

            let error = catalog.migrate_relation_namespace().unwrap_err();
            assert!(error.to_string().contains("migration collision"));
            assert!(error.to_string().contains("public.docs"));
            assert!(store
                .scan_prefix(&key_with_tag(TAG_RELATION))
                .unwrap()
                .is_empty());
            assert!(store
                .get(&single_str_key(TAG_TABLE, "docs").unwrap())
                .unwrap()
                .is_some());
        }
    }
}
