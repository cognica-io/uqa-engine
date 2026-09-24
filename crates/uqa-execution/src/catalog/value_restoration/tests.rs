//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod context;
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};
use uqa_core::{ArrayValue, LegacyVectorKind, LegacyVectorValue, RelationIdentity, Value};
use uqa_storage::{
    catalog::{ColumnStatsInput, RelationSecurityRow, TableSchema},
    DocumentMetadata, KeyValueCatalog, KeyValueStorageBackend, KeyValueStore, StoredDocument,
    ValueIndexEntry, ValueIndexKey,
};

const TABLE: &str = "public.legacy_items";

struct Fixture {
    store: Arc<dyn KeyValueStore>,
    catalog: KeyValueCatalog,
    backend: KeyValueStorageBackend,
}

impl Fixture {
    fn new(rows: u64) -> Self {
        let store: Arc<dyn KeyValueStore> = Arc::new(context::ControlledStore::new());
        let catalog = KeyValueCatalog::new(store.clone());
        catalog.save_schema("public").unwrap();
        let backend = KeyValueStorageBackend::new(store.clone());
        let uqa_sql::Statement::CreateTable(table) =
            uqa_sql::compile("CREATE TABLE legacy_items(k int2vector, untouched integer[])")
                .unwrap()
                .remove(0)
        else {
            panic!("expected a table declaration");
        };
        catalog
            .save_table(&TableSchema {
                relation: RelationIdentity::new("public", "legacy_items"),
                security: RelationSecurityRow::legacy("uqa"),
                object_id: [1; 16],
                storage_generation: [2; 16],
                analyzer_json: "{}".into(),
                fts_fields: Vec::new(),
                vector_fields: Vec::new(),
                columns_json: serde_json::to_string(&table.columns).unwrap(),
                constraints_json: "{}".into(),
            })
            .unwrap();
        catalog
            .save_catalog_index_row(&index_row(
                "legacy_key",
                TABLE,
                Some(uqa_sql::ColumnType::Int2Vector),
                false,
            ))
            .unwrap();
        let mut documents = backend.document_store(TABLE);
        let mut keys = Vec::new();
        for id in 1..=rows {
            let elements = vec![Value::Int(i64::try_from(id).unwrap())];
            let old = if id % 2 == 0 {
                Value::Array(ArrayValue::try_new(elements).unwrap())
            } else {
                Value::List(elements)
            };
            keys.push((id, old.clone()));
            documents
                .put_stored(
                    id,
                    StoredDocument::with_metadata(
                        BTreeMap::from([
                            ("k".into(), old),
                            (
                                "untouched".into(),
                                Value::Array(ArrayValue::try_new(vec![Value::Int(7)]).unwrap()),
                            ),
                        ]),
                        DocumentMetadata::with_tuple_xmin(400 + u32::try_from(id).unwrap()),
                    ),
                )
                .unwrap();
        }
        backend
            .replace_btree_index(TABLE, &physical_key(), &keys)
            .unwrap();
        catalog
            .save_column_stats(ColumnStatsInput::basic(
                TABLE,
                "k",
                i64::try_from(rows).unwrap(),
                0,
                Some("old minimum"),
                Some("old maximum"),
                i64::try_from(rows).unwrap(),
            ))
            .unwrap();
        Self {
            store,
            catalog,
            backend,
        }
    }

    fn restore(&self, fail: bool) -> Rebuild<'_> {
        Rebuild {
            fixture: self,
            calls: RefCell::new(Vec::new()),
            fail,
        }
    }

    fn row(&self, id: u64) -> StoredDocument {
        self.backend
            .document_store(TABLE)
            .get_stored(id)
            .unwrap()
            .unwrap()
    }
}

fn physical_key() -> ValueIndexKey {
    ValueIndexKey::Index("stable-physical-address".into())
}

fn index_row(
    name: &str,
    table: &str,
    ty: Option<uqa_sql::ColumnType>,
    expression: bool,
) -> CatalogIndexRow {
    let keys = vec![if expression {
        IndexKey::Expression(Box::new(uqa_sql::ast::Expr::Column("k".into())))
    } else {
        IndexKey::Column("k".into())
    }];
    CatalogIndexRow {
        relation: RelationIdentity::new("public", name),
        index_type: "btree".into(),
        table_name: table.into(),
        columns_json: serde_json::to_string(&keys).unwrap(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&super::super::index::IndexDefinition {
                key_types: ty.into_iter().collect(),
                ..Default::default()
            })
            .unwrap(),
        ),
    }
}

struct Rebuild<'a> {
    fixture: &'a Fixture,
    calls: RefCell<Vec<String>>,
    fail: bool,
}

impl ValueRestorationSession for Rebuild<'_> {
    fn index_build_context(&self) -> crate::schema::indexes::IndexBuildContext<'_> {
        crate::schema::indexes::IndexBuildContext {
            catalog: self.fixture,
            reads: self.fixture,
            expressions: crate::mutation::constraints::index_keys::IndexExpressionContext {
                catalog: self.fixture,
                expressions: self.fixture,
            },
            memory: self.fixture,
        }
    }

    fn rebuild_value_indexes_and_refresh_statistics(
        &self,
        table: &str,
    ) -> StorageBackendResult<()> {
        assert!(self.fixture.backend.btree_index_fields(table)?.is_empty());
        assert!(self.fixture.catalog.load_column_stats(table)?.is_empty());
        self.calls.borrow_mut().push(table.into());
        if self.fail {
            return Err(invalid("injected physical rebuild failure"));
        }
        let documents = self.fixture.backend.document_store(table);
        let values = documents
            .doc_ids()?
            .into_iter()
            .map(|id| Ok((id, documents.get_stored(id)?.unwrap().fields()["k"].clone())))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        self.fixture
            .backend
            .replace_btree_index(table, &physical_key(), &values)
    }
}

#[test]
fn restoration_preserves_paged_rows_metadata_and_index_identity() {
    let fixture = Fixture::new(257);
    let index = fixture.catalog.load_catalog_indexes().unwrap().remove(0);
    let restore = fixture.restore(false);
    let control = fixture.backend.retention_control().unwrap();
    let before = control.memory().used();
    fixture.backend.begin_transaction().unwrap();
    normalize_legacy_vectors(&fixture.catalog, &fixture.backend, &restore).unwrap();
    fixture.backend.commit_transaction().unwrap();
    assert_eq!(&*restore.calls.borrow(), &[TABLE]);
    assert_eq!(control.memory().used(), before);
    assert_eq!(
        fixture
            .catalog
            .get_metadata(VERSION_KEY)
            .unwrap()
            .as_deref(),
        Some("1")
    );
    for id in 1..=257 {
        let row = fixture.row(id);
        let expected = Value::LegacyVector(
            LegacyVectorValue::try_new(
                LegacyVectorKind::SmallInteger,
                vec![Value::Int(i64::try_from(id).unwrap())],
            )
            .unwrap(),
        );
        assert_eq!(row.fields()["k"], expected);
        assert_eq!(
            row.metadata().tuple_xmin(),
            Some(400 + u32::try_from(id).unwrap())
        );
        assert!(
            matches!(&row.fields()["untouched"], Value::Array(array) if array.lower_bounds() == [1])
        );
        assert_eq!(
            fixture
                .backend
                .read_btree_index_entry(TABLE, &physical_key(), id)
                .unwrap(),
            ValueIndexEntry::Present(expected)
        );
    }
    let current = fixture.catalog.load_catalog_indexes().unwrap().remove(0);
    assert_eq!(current.relation, index.relation);
    assert_eq!(current.definition_json, index.definition_json);
    assert!(fixture.catalog.load_column_stats(TABLE).unwrap().is_empty());
    fixture.backend.begin_transaction().unwrap();
    normalize_legacy_vectors(&fixture.catalog, &fixture.backend, &restore).unwrap();
    assert!(!fixture.store.transaction_has_written().unwrap());
    fixture.backend.commit_transaction().unwrap();
    assert_eq!(&*restore.calls.borrow(), &[TABLE]);
}

#[test]
fn failed_rebuild_rolls_back_rows_indexes_statistics_and_version() {
    let fixture = Fixture::new(2);
    let rows = [fixture.row(1), fixture.row(2)];
    let statistics = fixture.catalog.load_column_stats(TABLE).unwrap();
    let restore = fixture.restore(true);
    let control = fixture.backend.retention_control().unwrap();
    let before = control.memory().used();
    fixture.backend.begin_transaction().unwrap();
    let error = normalize_legacy_vectors(&fixture.catalog, &fixture.backend, &restore).unwrap_err();
    assert_eq!(error.to_string(), "injected physical rebuild failure");
    assert_eq!(fixture.catalog.get_metadata(VERSION_KEY).unwrap(), None);
    assert!(matches!(
        fixture.row(1).fields()["k"],
        Value::LegacyVector(_)
    ));
    fixture.backend.rollback_transaction().unwrap();
    assert_eq!(control.memory().used(), before);
    assert_eq!([fixture.row(1), fixture.row(2)], rows);
    assert_eq!(
        fixture.catalog.load_column_stats(TABLE).unwrap(),
        statistics
    );
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            fixture
                .backend
                .read_btree_index_entry(TABLE, &physical_key(), u64::try_from(index).unwrap() + 1)
                .unwrap(),
            ValueIndexEntry::Present(row.fields()["k"].clone())
        );
    }
    assert_eq!(fixture.catalog.get_metadata(VERSION_KEY).unwrap(), None);
}

#[test]
fn restored_unique_keys_are_validated_before_rebuilding_or_marking_completion() {
    let fixture = Fixture::new(2);
    let duplicate = fixture.row(1);
    fixture
        .backend
        .document_store(TABLE)
        .put_stored(2, duplicate.clone())
        .unwrap();
    let mut index = fixture.catalog.load_catalog_indexes().unwrap().remove(0);
    let mut definition = super::super::index::index_definition(&index).unwrap();
    definition.unique = true;
    index.definition_json = Some(serde_json::to_string(&definition).unwrap());
    fixture.catalog.save_catalog_index_row(&index).unwrap();
    let restore = fixture.restore(false);
    fixture.backend.begin_transaction().unwrap();
    let error = normalize_legacy_vectors(&fixture.catalog, &fixture.backend, &restore).unwrap_err();
    assert!(error.to_string().contains("legacy_key"), "{error}");
    assert!(restore.calls.borrow().is_empty());
    assert_eq!(fixture.catalog.get_metadata(VERSION_KEY).unwrap(), None);
    fixture.backend.rollback_transaction().unwrap();
    assert_eq!(fixture.row(1), duplicate);
    assert_eq!(fixture.row(2), duplicate);
    assert_eq!(
        fixture.backend.retention_control().unwrap().memory().used(),
        0
    );
}

#[test]
fn restoration_uses_the_backend_allowance_and_preserves_rows_on_exhaustion() {
    let fixture = Fixture::new(2);
    let original = fixture.row(1);
    let restore = fixture.restore(false);
    let control = fixture.backend.retention_control().unwrap();
    let exhausted = control.memory().reserve(control.memory().limit()).unwrap();
    fixture.backend.begin_transaction().unwrap();
    assert!(matches!(
        normalize_legacy_vectors(&fixture.catalog, &fixture.backend, &restore),
        Err(StorageBackendError::Memory(_))
    ));
    fixture.backend.rollback_transaction().unwrap();
    assert!(restore.calls.borrow().is_empty());
    assert_eq!(fixture.row(1), original);
    assert_eq!(fixture.catalog.get_metadata(VERSION_KEY).unwrap(), None);
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(exhausted);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn restoration_requires_its_transaction_and_rejects_unknown_versions() {
    let fixture = Fixture::new(1);
    let original = fixture.row(1);
    let restore = fixture.restore(false);
    assert!(
        normalize_legacy_vectors(&fixture.catalog, &fixture.backend, &restore)
            .unwrap_err()
            .to_string()
            .contains("initial transaction")
    );
    fixture.catalog.set_metadata(VERSION_KEY, "2").unwrap();
    fixture.backend.begin_transaction().unwrap();
    assert!(
        normalize_legacy_vectors(&fixture.catalog, &fixture.backend, &restore)
            .unwrap_err()
            .to_string()
            .contains("unsupported legacy vector carrier version")
    );
    assert!(!fixture.store.transaction_has_written().unwrap());
    fixture.backend.rollback_transaction().unwrap();
    assert!(restore.calls.borrow().is_empty());
    assert_eq!(fixture.row(1), original);
}

#[test]
fn expression_rebuilds_use_result_types_and_accept_legacy_table_names() {
    let rows = [
        index_row(
            "typed",
            "typed_table",
            Some(uqa_sql::ColumnType::OidVector),
            true,
        ),
        index_row("old", "old_table", None, true),
        index_row(
            "ordinary",
            "ordinary_table",
            Some(uqa_sql::ColumnType::Integer),
            true,
        ),
        index_row("column", "column_table", None, false),
    ];
    assert_eq!(
        expression_index_tables(&rows).unwrap(),
        BTreeSet::from(["public.typed_table".into(), "public.old_table".into(),])
    );
}
