//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete unique keys must not read documents that only share a key prefix.

use super::*;
use crate::tests::relation_lock_support::{error, sql};
use std::path::Path;
use std::sync::atomic::Ordering;
use uqa_execution::mutation::constraints::index_keys::EnforcedKeyExecution;
use uqa_storage::ValueIndexKey;

#[rstest::rstest]
#[case::primary_key(", PRIMARY KEY (tenant_id, item_id)", "")]
#[case::unique_constraint(", UNIQUE (tenant_id, item_id)", "")]
#[case::unique_index("", "CREATE UNIQUE INDEX item_key ON items(tenant_id, item_id)")]
#[case::partial_unique_index(
    "",
    "CREATE UNIQUE INDEX item_key ON items(tenant_id, item_id) WHERE active"
)]
fn composite_unique_inserts_do_not_read_rows_sharing_the_leading_key(
    #[case] constraint: &str,
    #[case] index: &str,
) {
    let mut reads = Vec::new();
    for count in [32, 128] {
        let engine = Engine::new();
        engine
            .sql(
                &format!("CREATE TABLE items (tenant_id INTEGER, item_id INTEGER, active BOOLEAN, payload TEXT{constraint}); {index}"),
                &[],
            )
            .unwrap();
        engine
            .sql(
                &format!("INSERT INTO items SELECT 1, i, true, repeat('x',128) FROM generate_series(1,{count}) g(i)"),
                &[],
            )
            .unwrap();
        let probe = PortalSnapshotProbeStore::from_table(&engine, "items");
        let fields = Arc::clone(&probe.field_reads);
        let rows = Arc::clone(&probe.row_reads);
        *engine
            .table("items")
            .unwrap()
            .unwrap()
            .document_store
            .write() = Box::new(probe);
        engine
            .sql(
                "INSERT INTO items VALUES (1, $1, true, 'new item')",
                &[SQLParam::scalar(Value::Int(count + 1))],
            )
            .unwrap();
        reads.push((fields.load(Ordering::Relaxed), rows.load(Ordering::Relaxed)));
        assert_eq!(
            engine
                .sql("SELECT count(*) AS n FROM items", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(count + 1),
        );
    }
    assert_eq!(
        reads,
        [(0, 0), (0, 0)],
        "(field reads, row reads) at 32 and 128 stored rows"
    );
}

fn open(provider: usize, path: &Path) -> Engine {
    let engine = match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine
}

// Results and SQLSTATEs independently checked with PostgreSQL 18.4.
#[rstest::rstest]
fn composite_unique_keys_preserve_partial_null_and_rollback_semantics(
    #[values(0, 1, 2, 3)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unique.db");
    let engine = open(provider, &path);
    sql(&engine, "CREATE TABLE items(tenant_id int, item_id int, active boolean, payload text); CREATE UNIQUE INDEX item_key ON items(tenant_id,item_id) WHERE active; INSERT INTO items VALUES(1,1,true,'first'),(1,2,true,'second'),(2,1,true,'other tenant'),(1,1,false,'inactive'),(1,1,false,'inactive again')");
    error(
        &engine,
        "INSERT INTO items VALUES(1,3,true,'third'),(1,3,true,'duplicate')",
        "23505",
    );
    assert_eq!(
        sql(&engine, "SELECT count(*) AS n FROM items").rows[0]["n"],
        Value::Int(5)
    );
    sql(&engine, "INSERT INTO items VALUES(1,1,true,'changed') ON CONFLICT(tenant_id,item_id) WHERE active DO UPDATE SET payload=excluded.payload; UPDATE items SET item_id=3 WHERE tenant_id=1 AND item_id=2 AND active");
    error(
        &engine,
        "UPDATE items SET item_id=1 WHERE tenant_id=1 AND item_id=3 AND active",
        "23505",
    );
    assert_eq!(
        sql(
            &engine,
            "SELECT payload FROM items WHERE tenant_id=1 AND item_id=1 AND active"
        )
        .rows[0]["payload"],
        s("changed")
    );
    assert_eq!(
        sql(&engine, "SELECT item_id FROM items WHERE payload='second'").rows[0]["item_id"],
        Value::Int(3)
    );
    sql(&engine, "CREATE TABLE distinct_keys(a int,b int,UNIQUE(a,b)); INSERT INTO distinct_keys VALUES(1,NULL),(1,NULL),(NULL,1),(NULL,1)");
    assert_eq!(
        sql(&engine, "SELECT count(*) AS n FROM distinct_keys").rows[0]["n"],
        Value::Int(4)
    );
    sql(&engine, "CREATE TABLE null_keys(a int,b int,UNIQUE NULLS NOT DISTINCT(a,b)); INSERT INTO null_keys VALUES(1,NULL),(NULL,1),(NULL,NULL)");
    error(&engine, "INSERT INTO null_keys VALUES(1,NULL)", "23505");
    error(&engine, "INSERT INTO null_keys VALUES(NULL,NULL)", "23505");
    sql(&engine, "BEGIN ISOLATION LEVEL REPEATABLE READ; INSERT INTO items VALUES(1,4,true,'private'); SAVEPOINT kept; UPDATE items SET item_id=5 WHERE payload='private'; ROLLBACK TO kept; INSERT INTO items VALUES(1,4,true,'ignored') ON CONFLICT(tenant_id,item_id) WHERE active DO NOTHING; COMMIT");
    assert_eq!(
        sql(&engine, "SELECT count(*) AS n FROM items").rows[0]["n"],
        Value::Int(6)
    );
    sql(&engine, "CREATE TABLE partitioned(tenant_id int, item_id int, payload text, UNIQUE(tenant_id,item_id)) PARTITION BY RANGE(tenant_id); CREATE TABLE child PARTITION OF partitioned FOR VALUES FROM(0) TO(10); INSERT INTO partitioned VALUES(1,1,'old'); INSERT INTO partitioned VALUES(1,1,'new') ON CONFLICT(tenant_id,item_id) DO UPDATE SET payload=excluded.payload");
    assert_eq!(
        sql(&engine, "SELECT payload FROM child").rows[0]["payload"],
        s("new")
    );
    if provider != 0 {
        drop(engine);
        let reopened = open(provider, &path);
        error(
            &reopened,
            "INSERT INTO items VALUES(1,4,true,'duplicate')",
            "23505",
        );
        error(
            &reopened,
            "INSERT INTO null_keys VALUES(NULL,NULL)",
            "23505",
        );
        sql(&reopened, "INSERT INTO partitioned VALUES(1,1,'reopened') ON CONFLICT(tenant_id,item_id) DO UPDATE SET payload=excluded.payload");
        assert_eq!(
            sql(&reopened, "SELECT payload FROM child").rows[0]["payload"],
            s("reopened")
        );
    }
}

#[test]
fn composite_unique_command_keys_mask_stored_rows_and_ignored_updates() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE items(a int,b int,active boolean); CREATE UNIQUE INDEX item_key ON items(a,b) WHERE active; INSERT INTO items VALUES(1,1,true)");
    let key = engine.enforced_keys("items").unwrap().remove(0);
    let find = |b, ignored| {
        key.find_conflict(
            engine.constraint_execution_context(),
            "items",
            &[Value::Int(1), Value::Int(b)],
            ignored,
        )
        .unwrap()
    };
    let id = find(1, None).unwrap();
    let row = |active| {
        doc([
            ("a", Value::Int(1)),
            ("b", Value::Int(2)),
            ("active", Value::Bool(active)),
        ])
    };
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine
        .stage_command_document("items", id, Some(row(false)))
        .unwrap();
    assert_eq!(find(1, None), None);
    assert_eq!(find(2, None), None);
    engine
        .stage_command_document("items", id, Some(row(true)))
        .unwrap();
    assert_eq!(find(2, None), Some(id));
    assert_eq!(find(2, Some(id)), None);
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine.stage_command_document("items", id, None).unwrap();
    assert_eq!(find(2, None), None);
    let other = id + 1;
    engine
        .stage_command_document("items", other, Some(row(true)))
        .unwrap();
    assert_eq!(find(2, Some(id)), Some(other));
    engine.mutation_coordinator().end_command_mutation_overlay();
    assert_eq!(find(2, None), Some(id));
    engine.mutation_coordinator().end_command_mutation_overlay();
    assert_eq!(find(1, None), Some(id));
    assert_eq!(find(2, None), None);
}

#[rstest::rstest]
fn composite_unique_indexes_repair_predecessor_column_only_postings_on_open(
    #[values(1, 2, 3)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("upgrade.db");
    let engine = open(provider, &path);
    sql(
        &engine,
        "CREATE TABLE items(a int,b int,UNIQUE(a,b)); INSERT INTO items VALUES(1,1),(1,2),(2,1)",
    );
    let key = engine.enforced_keys("items").unwrap().remove(0);
    let physical = ValueIndexKey::Index(key.index_catalog.unwrap().physical_key);
    let backend = engine.storage.backend.as_ref().unwrap();
    assert!(backend
        .load_btree_index("public.items", &physical)
        .unwrap()
        .is_some());
    backend.begin_transaction().unwrap();
    backend.drop_btree_index("public.items", &physical).unwrap();
    backend.commit_transaction().unwrap();
    drop(engine);
    let restored = open(provider, &path);
    let postings = restored
        .storage
        .backend
        .as_ref()
        .unwrap()
        .load_btree_index("public.items", &physical)
        .unwrap()
        .unwrap();
    assert_eq!(postings.len(), 3);
    assert!(postings
        .iter()
        .all(|(_, value)| matches!(value, Value::Row(_))));
    error(&restored, "INSERT INTO items VALUES(1,2)", "23505");
    sql(&restored, "INSERT INTO items VALUES(1,3)");
    drop(restored);
    let reopened = open(provider, &path);
    error(&reopened, "INSERT INTO items VALUES(1,3)", "23505");
    assert_eq!(
        sql(&reopened, "SELECT count(*) AS n FROM items").rows[0]["n"],
        Value::Int(4)
    );
}
