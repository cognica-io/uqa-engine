//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_execution::schema::constraints::restoration::RELATION_ATTRIBUTE_METADATA_KEY;

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn query_defined_columns_retain_attributes_across_sessions_and_reopen(#[case] provider: usize) {
    let (_directory, first, second) = sessions(provider);
    for (name, suffix) in [("ctas_rows", ""), ("ctas_empty", "WITH NO DATA")] {
        sql(
            &first,
            &format!("CREATE TABLE {name} AS SELECT 7 AS a, 'retained'::text AS b {suffix}"),
        );
        let columns = first.table(name).unwrap().unwrap().columns.read().clone();
        assert_eq!(
            columns
                .iter()
                .map(|column| column.attribute_number)
                .collect::<Vec<_>>(),
            [Some(1), Some(2)]
        );
        assert!(columns.iter().all(|column| column.object_id.is_some()));
        let query = format!("SELECT attname,attnum FROM pg_attribute WHERE attrelid='{name}'::regclass AND attnum>0 ORDER BY attnum");
        let expected = sql(&first, &query).rows;
        assert_eq!(expected.len(), 2);
        assert_eq!(expected[0]["attnum"], uqa_core::Value::Int(1));
        assert_eq!(expected[1]["attnum"], uqa_core::Value::Int(2));
        assert_eq!(sql(&second, &query).rows, expected);
    }
    assert_eq!(
        sql(&second, "SELECT count(*) AS n FROM ctas_rows").rows[0]["n"],
        uqa_core::Value::Int(1)
    );
    assert_eq!(
        sql(&second, "SELECT count(*) AS n FROM ctas_empty").rows[0]["n"],
        uqa_core::Value::Int(0)
    );
    sql(
        &first,
        "ALTER TABLE ctas_rows DROP COLUMN a; ALTER TABLE ctas_rows ADD COLUMN c integer DEFAULT 9",
    );
    let query = "SELECT attname,attnum,attisdropped FROM pg_attribute WHERE attrelid='ctas_rows'::regclass AND attnum>0 ORDER BY attnum";
    let expected = sql(&first, query).rows;
    assert_eq!(expected.len(), 3);
    assert_eq!(expected[0]["attisdropped"], uqa_core::Value::Bool(true));
    assert_eq!(expected[2]["attnum"], uqa_core::Value::Int(3));
    let identities = first
        .table("ctas_rows")
        .unwrap()
        .unwrap()
        .columns
        .read()
        .iter()
        .map(|column| (column.object_id, column.attribute_number))
        .collect::<Vec<_>>();
    let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
    drop((first, second));
    let reopened = Engine::from_persistent_provider(factory).unwrap();
    assert_eq!(sql(&reopened, query).rows, expected);
    assert_eq!(
        reopened
            .table("ctas_rows")
            .unwrap()
            .unwrap()
            .columns
            .read()
            .iter()
            .map(|column| (column.object_id, column.attribute_number))
            .collect::<Vec<_>>(),
        identities
    );
    let rows = sql(&reopened, "SELECT b,c FROM ctas_rows").rows;
    assert_eq!(rows[0]["b"], uqa_core::Value::Str("retained".into()));
    assert_eq!(rows[0]["c"], uqa_core::Value::Int(9));
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn legacy_attribute_layouts_migrate_atomically_and_survive_peer_restore(#[case] provider: usize) {
    let (_directory, first, second) = sessions(provider);
    sql(&first, "CREATE TABLE attribute_legacy(a integer,b text); INSERT INTO attribute_legacy VALUES(7,'retained')");
    let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
    let raw = factory.open_session().unwrap();
    drop((first, second));
    raw.backend.begin_transaction().unwrap();
    for mut row in raw.catalog.load_tables().unwrap() {
        let mut columns: Vec<uqa_sql::ast::ColumnDef> =
            serde_json::from_str(&row.columns_json).unwrap();
        for column in &mut columns {
            column.attribute_number = None;
        }
        row.columns_json = serde_json::to_string(&columns).unwrap();
        raw.catalog.save_table(&row).unwrap();
    }
    raw.catalog
        .delete_metadata(RELATION_ATTRIBUTE_METADATA_KEY)
        .unwrap();
    raw.catalog.set_metadata("sql_functions_json", "{").unwrap();
    raw.backend.commit_transaction().unwrap();
    let before: Vec<_> = raw
        .catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .map(|row| (row.relation, row.columns_json))
        .collect();
    assert!(Engine::from_persistent_provider(Arc::clone(&factory)).is_err());
    let after: Vec<_> = raw
        .catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .map(|row| (row.relation, row.columns_json))
        .collect();
    assert_eq!(after, before);
    assert!(raw
        .catalog
        .get_metadata(RELATION_ATTRIBUTE_METADATA_KEY)
        .unwrap()
        .is_none());
    raw.catalog.delete_metadata("sql_functions_json").unwrap();
    let restored = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    let initial = sql(&restored, "SELECT attname,attnum FROM pg_attribute WHERE attrelid='attribute_legacy'::regclass AND attnum>0 ORDER BY attnum").rows;
    assert_eq!(initial.len(), 2);
    assert_eq!(initial[0]["attnum"], uqa_core::Value::Int(1));
    assert_eq!(initial[1]["attnum"], uqa_core::Value::Int(2));
    sql(&restored, "ALTER TABLE attribute_legacy DROP COLUMN a; ALTER TABLE attribute_legacy ADD COLUMN c integer DEFAULT 9");
    let query = "SELECT attname,attnum,attisdropped FROM pg_attribute WHERE attrelid='attribute_legacy'::regclass AND attnum>0 ORDER BY attnum";
    let expected = sql(&restored, query).rows;
    assert_eq!(sql(&restored.new_session().unwrap(), query).rows, expected);
    drop(restored);
    let reopened = Engine::from_persistent_provider(factory).unwrap();
    assert_eq!(sql(&reopened, query).rows, expected);
    let rows = sql(&reopened, "SELECT b,c FROM attribute_legacy").rows;
    assert_eq!(rows[0]["b"], uqa_core::Value::Str("retained".into()));
    assert_eq!(rows[0]["c"], uqa_core::Value::Int(9));
}
