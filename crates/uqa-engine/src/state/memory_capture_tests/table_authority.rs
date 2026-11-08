//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native authority generations share original image ownership through ordinary SQL state transitions.

use super::{sql, Engine};
use crate::tests::providers::Provider;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl, RetainedValue},
    CancellationToken,
};
use uqa_execution::catalog::security::table_inquiry::TablePrivilegeState;
use uqa_sql::catalog::{security::BoundTableSecurity, table_metadata::TableMetadata};

fn image(value: &BoundTableSecurity) -> Vec<u8> {
    let token = CancellationToken::new();
    let mut reader = value.image_reader(&token).unwrap();
    let mut bytes = vec![];
    let mut buffer = [0; 23];
    loop {
        let n = reader.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    bytes
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn admitted_table_authority_survives_sql_rollback_and_retained_reader_retirement(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine: Engine = provider.open(&directory.path().join("authority.db"));
    sql(&engine, "CREATE ROLE reader; CREATE TABLE saved(id integer, body text); GRANT SELECT(body) ON saved TO reader");
    let table = engine.table("saved").unwrap().unwrap();
    let expected = image(&table.security.read());
    let input = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let decoded = BoundTableSecurity::read_image(&mut expected.as_slice(), &input, &token).unwrap();
    let pointer = decoded.column_acls.keys().next().unwrap().as_ptr();
    let generation = RetainedValue::with_budget(
        TableMetadata::from_admitted(decoded, &token).unwrap(),
        &input,
    )
    .unwrap();
    table.security.restore(&generation);
    let original_generation = table.security.generation();
    assert!(RetainedValue::ptr_eq(&generation, &original_generation));
    drop(generation);
    let retained = table
        .security_snapshot_with_control(&ProductionControl::uncontrolled())
        .unwrap();
    assert_eq!(
        retained.column_acls.keys().next().unwrap().as_ptr(),
        pointer
    );
    token.cancel();
    sql(
        &engine,
        "BEGIN; REVOKE SELECT(body) ON saved FROM reader; ROLLBACK",
    );
    assert_eq!(
        image(&engine.table("saved").unwrap().unwrap().security.read()),
        expected
    );
    let restored_generation = engine
        .table("saved")
        .unwrap()
        .unwrap()
        .security
        .generation();
    sql(&engine, "REVOKE SELECT(body) ON saved FROM reader");
    assert!(!RetainedValue::ptr_eq(
        &restored_generation,
        &engine
            .table("saved")
            .unwrap()
            .unwrap()
            .security
            .generation()
    ));
    drop(restored_generation);
    assert_ne!(
        image(&engine.table("saved").unwrap().unwrap().security.read()),
        expected
    );
    assert_eq!(image(&retained), expected);
    sql(&engine, "DROP TABLE saved; DROP ROLE reader");
    drop((table, engine));
    assert!(input.used() > 0);
    assert_eq!(image(&retained), expected);
    drop(retained);
    assert!(input.used() > 0);
    drop(original_generation);
    assert_eq!(input.used(), 0);
}
