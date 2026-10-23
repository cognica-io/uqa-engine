//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy document indexes retain their catalog attributes and table dependencies without a declared row schema.

use super::*;

#[test]
fn legacy_document_index_does_not_block_catalog_reads_or_unrelated_table_deletion() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE documents ()");
        first
            .register_catalog_index(
                "document_owner_idx",
                "btree",
                "documents",
                &["owner_id".into()],
                &[],
            )
            .unwrap();
        let original = definition(&first, "document_owner_idx");
        assert!(original.key_types.is_empty());
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        let attributes = sql(
            &reopened,
            "SELECT attname, atttypid FROM pg_attribute WHERE attrelid = 'document_owner_idx'::regclass ORDER BY attnum",
        );
        assert_eq!(attributes.rows.len(), 1);
        assert_eq!(attributes.rows[0]["attname"], Value::Str("owner_id".into()));
        assert_eq!(attributes.rows[0]["atttypid"], Value::Int(25));
        let dependency = sql(
            &reopened,
            "SELECT deptype FROM pg_depend WHERE classid = 'pg_class'::regclass AND objid = 'document_owner_idx'::regclass AND refclassid = 'pg_class'::regclass AND refobjid = 'documents'::regclass AND refobjsubid = 0",
        );
        assert_eq!(dependency.rows.len(), 1);
        assert_eq!(dependency.rows[0]["deptype"], Value::Str("a".into()));
        sql(&reopened, "BEGIN; DROP TABLE t; ROLLBACK");
        assert_eq!(
            sql(&reopened, "SELECT v FROM t").rows[0]["v"],
            Value::Int(1)
        );
        sql(&reopened, "DROP TABLE t");
        assert_eq!(definition(&reopened, "document_owner_idx"), original);
        sql(&reopened, "BEGIN; DROP TABLE documents; ROLLBACK");
        assert_eq!(definition(&reopened, "document_owner_idx"), original);
        sql(&reopened, "DROP TABLE documents");
        assert!(reopened
            .catalog_index("document_owner_idx")
            .unwrap()
            .is_none());
    }
}

#[test]
fn document_index_included_fields_follow_sql_binding_catalog_types() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE documents (); CREATE INDEX owner_idx ON documents(owner_id) INCLUDE(extra)");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        let attributes = sql(&reopened, "SELECT attname, atttypid FROM pg_attribute WHERE attrelid = 'owner_idx'::regclass ORDER BY attnum");
        assert_eq!(attributes.rows.len(), 2);
        for (row, name) in attributes.rows.iter().zip(["owner_id", "extra"]) {
            assert_eq!(row["attname"], Value::Str(name.into()));
            assert_eq!(row["atttypid"], Value::Int(25));
        }
        sql(&reopened, "DROP TABLE documents");
    }
}

#[test]
fn declared_index_attributes_preserve_postgresql_types_and_creation_names() {
    // PostgreSQL 18.4: (1,id,23), (2,lower,25), (3,extra,20); rollback retains the index.
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE typed_documents(id integer, owner_id text, extra bigint); CREATE INDEX typed_owner_idx ON typed_documents(id,lower(owner_id)) INCLUDE(extra); ALTER TABLE typed_documents RENAME COLUMN id TO document_id");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        let attributes = sql(&reopened, "SELECT attname, atttypid FROM pg_attribute WHERE attrelid = 'typed_owner_idx'::regclass ORDER BY attnum");
        assert_eq!(attributes.rows.len(), 3);
        for (row, (name, oid)) in
            attributes
                .rows
                .iter()
                .zip([("id", 23), ("lower", 25), ("extra", 20)])
        {
            assert_eq!(row["attname"], Value::Str(name.into()));
            assert_eq!(row["atttypid"], Value::Int(oid));
        }
        sql(&reopened, "BEGIN; DROP TABLE typed_documents; ROLLBACK");
        assert!(reopened.catalog_index("typed_owner_idx").unwrap().is_some());
    }
}
