//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy catalog namespace discovery and atomic relation migration.

use super::physical_indexes::table_index_prefixes;
use super::records::{
    LegacySequenceState, LegacyTableSchema, StoredCatalogIndex, StoredForeignTable, StoredRelation,
    StoredSequence, StoredView, LEGACY_SEQUENCES_METADATA_KEY, LEGACY_VIEWS_METADATA_KEY,
};
use super::{
    batch_rekey_prefix, column_stats_prefix, decode_catalog_relation_key, decode_relation_key,
    decode_value, doc_length_key_prefix, document_key_prefix, encode_value, ensure_prefix_absent,
    field_stats_key_prefix, key_with_tag, posting_cluster_positions_key_prefix,
    posting_cluster_score_key_prefix, posting_document_key_prefix, posting_key_prefix,
    register_migration_relation, relation_key, reverse_posting_key_prefix, single_str_key,
    string_value, table_field_analyzer_prefix, vector_key_prefix, CatalogFacade, KeyValueBatch,
    KeyValueCatalog, KeyValueStore, RelationIdentity, RelationKind, SequenceOptions, SequenceRow,
    StorageBackendError, StorageBackendResult, TableSchema, ViewRow, TAG_CATALOG_INDEX,
    TAG_FOREIGN_TABLE, TAG_METADATA, TAG_RELATION, TAG_SCHEMA, TAG_SEQUENCE, TAG_TABLE, TAG_VIEW,
};

pub(super) type SeenRelations =
    std::collections::BTreeMap<RelationIdentity, (RelationKind, String)>;

pub(super) struct TableMigration {
    old_key: Vec<u8>,
    old_physical_name: String,
    schema: TableSchema,
}

pub(super) struct SequenceMigration {
    old_key: Option<Vec<u8>>,
    row: SequenceRow,
}

pub(super) struct ForeignMigration {
    old_key: Vec<u8>,
    relation: RelationIdentity,
    value: Vec<u8>,
}

pub(super) struct ViewMigration {
    old_key: Option<Vec<u8>>,
    row: ViewRow,
}

enum IndexMigration {
    Delete(Vec<u8>),
    Put {
        old_key: Vec<u8>,
        new_key: Vec<u8>,
        stored: StoredCatalogIndex,
    },
}

pub(super) struct RelationMigrations {
    pub(super) seen: SeenRelations,
    tables: Vec<TableMigration>,
    sequences: Vec<SequenceMigration>,
    foreign_tables: Vec<ForeignMigration>,
    views: Vec<ViewMigration>,
    indexes: Vec<IndexMigration>,
}

pub(super) fn decode_migrated_table(
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
        role_owner: "uqa".into(),
        acl: None,
        object_id: [0; 16],
        storage_generation: [0; 16],
        analyzer_json: legacy.analyzer_json,
        fts_fields: legacy.fts_fields,
        vector_fields: legacy.vector_fields,
        columns_json: legacy.columns_json,
        constraints_json: legacy.constraints_json,
    })
}

pub(super) fn collect_table_migrations(
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

pub(super) fn collect_sequence_migrations(
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
                role_owner: stored.role_owner,
                acl: stored.acl,
                object_id: stored.object_id,
                definition_generation: stored.definition_generation,
                start: stored.start,
                increment: stored.increment,
                current: stored.current,
                called: stored.called,
                log_count: stored.log_count,
                persistence: stored.persistence,
                owner: stored.owner,
                options: stored.options,
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
                    role_owner: "uqa".into(),
                    acl: None,
                    object_id: [0; 16],
                    definition_generation: [0; 16],
                    start: state.start,
                    increment: state.increment,
                    current: state.current,
                    called: true,
                    log_count: 0,
                    persistence: "p".into(),
                    owner: None,
                    options: SequenceOptions::default(),
                },
            });
        }
    }
    Ok(sequences)
}

pub(super) fn collect_foreign_migrations(
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

pub(super) fn collect_view_migrations(
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

fn collect_index_migrations(
    store: &dyn KeyValueStore,
    seen: &mut SeenRelations,
    tables: &[TableMigration],
) -> StorageBackendResult<Vec<IndexMigration>> {
    let mut table_aliases = std::collections::BTreeMap::new();
    for table in tables {
        let canonical = table.schema.relation.qualified_name();
        table_aliases.insert(table.old_physical_name.clone(), canonical.clone());
        table_aliases.insert(canonical.clone(), canonical);
    }
    let mut indexes = Vec::new();
    for (key, value) in store.scan_prefix(&key_with_tag(TAG_CATALOG_INDEX))? {
        let (key_relation, legacy_key, source) = decode_catalog_relation_key(&key)?;
        let mut stored = decode_value::<StoredCatalogIndex>(&value)?;
        if let Some(canonical) = table_aliases.get(&stored.table_name) {
            stored.table_name.clone_from(canonical);
        }
        let table = RelationIdentity::from_legacy_name(&stored.table_name)
            .map_err(StorageBackendError::Other)?;
        if table.schema.starts_with("pg_temp_") {
            if !legacy_key {
                return Err(StorageBackendError::Other(format!(
                    "typed catalog index `{source}` cannot be stored in a temporary schema"
                )));
            }
            indexes.push(IndexMigration::Delete(key));
            continue;
        }
        if !matches!(seen.get(&table), Some((RelationKind::Table, _))) {
            return Err(StorageBackendError::Other(format!(
                "catalog index `{source}` references missing table `{}`",
                table.qualified_name()
            )));
        }
        let relation = if legacy_key {
            RelationIdentity::from_legacy_index_name(&source, &table)
        } else {
            if key_relation.schema != table.schema {
                return Err(StorageBackendError::Other(format!(
                    "catalog index key `{source}` disagrees with table schema `{}`",
                    table.schema
                )));
            }
            let parent = store
                .get(&relation_key(TAG_RELATION, &key_relation)?)?
                .ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "catalog index `{source}` has no index relation parent"
                    ))
                })?;
            let actual = decode_value::<StoredRelation>(&parent)?.kind;
            if actual != RelationKind::Index {
                return Err(StorageBackendError::Other(format!(
                    "catalog index `{source}` has a {} relation parent",
                    actual.as_str()
                )));
            }
            key_relation
        };
        register_migration_relation(seen, &relation, RelationKind::Index, source)?;
        stored.table_name = table.qualified_name();
        indexes.push(IndexMigration::Put {
            old_key: key,
            new_key: relation_key(TAG_CATALOG_INDEX, &relation)?,
            stored,
        });
    }
    Ok(indexes)
}

pub(super) fn collect_relation_migrations(
    catalog: &KeyValueCatalog,
) -> StorageBackendResult<RelationMigrations> {
    let mut seen = SeenRelations::new();
    let tables = collect_table_migrations(catalog.store.as_ref(), &mut seen)?;
    let sequences = collect_sequence_migrations(catalog, &mut seen)?;
    let foreign_tables = collect_foreign_migrations(catalog.store.as_ref(), &mut seen)?;
    let views = collect_view_migrations(catalog, &mut seen)?;
    let indexes = collect_index_migrations(catalog.store.as_ref(), &mut seen, &tables)?;
    Ok(RelationMigrations {
        seen,
        tables,
        sequences,
        foreign_tables,
        views,
        indexes,
    })
}

pub(super) fn validate_relation_parents(
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

pub(super) fn put_relation_parents(
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

pub(super) fn table_data_prefixes(table_name: &str) -> StorageBackendResult<Vec<Vec<u8>>> {
    let mut prefixes = vec![
        document_key_prefix(table_name)?,
        posting_key_prefix(table_name)?,
        posting_cluster_score_key_prefix(table_name)?,
        posting_cluster_positions_key_prefix(table_name)?,
        posting_document_key_prefix(table_name)?,
        doc_length_key_prefix(table_name)?,
        field_stats_key_prefix(table_name)?,
        reverse_posting_key_prefix(table_name)?,
        vector_key_prefix(table_name)?,
        column_stats_prefix(table_name)?,
        table_field_analyzer_prefix(table_name)?,
    ];
    prefixes.extend(table_index_prefixes(table_name)?);
    Ok(prefixes)
}

pub(super) fn apply_table_migration(
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

pub(super) fn put_sequence_migrations(
    batch: &mut dyn KeyValueBatch,
    sequences: Vec<SequenceMigration>,
) -> StorageBackendResult<()> {
    for sequence in sequences {
        let key = relation_key(TAG_SEQUENCE, &sequence.row.relation)?;
        batch.put(
            &key,
            &encode_value(&StoredSequence {
                role_owner: sequence.row.role_owner,
                acl: sequence.row.acl,
                object_id: sequence.row.object_id,
                definition_generation: sequence.row.definition_generation,
                start: sequence.row.start,
                increment: sequence.row.increment,
                current: sequence.row.current,
                called: sequence.row.called,
                log_count: sequence.row.log_count,
                persistence: sequence.row.persistence,
                owner: sequence.row.owner,
                options: sequence.row.options,
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

pub(super) fn put_foreign_migrations(
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

pub(super) fn put_view_migrations(
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

fn put_index_migrations(
    batch: &mut dyn KeyValueBatch,
    indexes: Vec<IndexMigration>,
) -> StorageBackendResult<()> {
    for index in indexes {
        match index {
            IndexMigration::Delete(key) => batch.delete(&key)?,
            IndexMigration::Put {
                old_key,
                new_key,
                stored,
            } => {
                batch.put(&new_key, &encode_value(&stored)?)?;
                if old_key != new_key {
                    batch.delete(&old_key)?;
                }
            }
        }
    }
    Ok(())
}

fn finish_relation_migration(batch: &mut dyn KeyValueBatch) -> StorageBackendResult<()> {
    for metadata_key in [LEGACY_VIEWS_METADATA_KEY, LEGACY_SEQUENCES_METADATA_KEY] {
        batch.put(
            &single_str_key(TAG_METADATA, metadata_key)?,
            &string_value("{}"),
        )?;
    }
    Ok(())
}

pub(super) fn apply_relation_migrations(
    catalog: &KeyValueCatalog,
    migrations: RelationMigrations,
) -> StorageBackendResult<()> {
    let RelationMigrations {
        seen,
        tables,
        sequences,
        foreign_tables,
        views,
        indexes,
    } = migrations;
    let mut batch = catalog.store.batch();
    put_relation_parents(catalog.store.as_ref(), batch.as_mut(), &seen)?;
    for table in tables {
        apply_table_migration(catalog.store.as_ref(), batch.as_mut(), table)?;
    }
    put_sequence_migrations(batch.as_mut(), sequences)?;
    put_foreign_migrations(batch.as_mut(), foreign_tables)?;
    put_view_migrations(batch.as_mut(), views)?;
    put_index_migrations(batch.as_mut(), indexes)?;
    finish_relation_migration(batch.as_mut())?;
    batch.commit()
}
