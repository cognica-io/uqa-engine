//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::{providers::Provider, relation_lock_support::sql};
use uqa_core::Value;

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn temporary_routines_survive_refresh_and_remain_absent_after_reopen(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporary-routines.db");
    let first = provider.open(&path);
    sql(
        &first,
        "CREATE FUNCTION pg_temp.session_function() RETURNS integer LANGUAGE SQL AS 'SELECT 7'",
    );
    sql(
        &first,
        "CREATE FUNCTION durable_function() RETURNS integer LANGUAGE SQL AS 'SELECT 11'",
    );
    let second = first.new_session().unwrap();
    sql(
        &second,
        "CREATE FUNCTION other_function() RETURNS integer LANGUAGE SQL AS 'SELECT 13'",
    );
    assert_eq!(
        sql(&first, "SELECT pg_temp.session_function() AS value").rows[0]["value"],
        Value::Int(7)
    );
    assert_eq!(
        sql(&first, "SELECT other_function() AS value").rows[0]["value"],
        Value::Int(13)
    );
    drop(second);
    drop(first);
    let reopened = provider.open(&path);
    assert_eq!(
        sql(
            &reopened,
            "SELECT count(*) AS value FROM pg_proc WHERE proname = 'session_function'"
        )
        .rows[0]["value"],
        Value::Int(0)
    );
    assert_eq!(
        sql(&reopened, "SELECT durable_function() AS value").rows[0]["value"],
        Value::Int(11)
    );
    assert_eq!(
        sql(&reopened, "SELECT other_function() AS value").rows[0]["value"],
        Value::Int(13)
    );
}
