//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture, prepare, providers::destination, repository};
use crate::{backup::RestoreOptions, tests::providers::Provider, Engine};
use uqa_core::Value;
use uqa_storage::read_control::StorageReadControl;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_key_value(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn full_and_incremental_restore_preserve_creation_privileges(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    source.sql("CREATE ROLE report_reader; CREATE SCHEMA reports; ALTER DEFAULT PRIVILEGES IN SCHEMA reports GRANT SELECT ON TABLES TO report_reader; CREATE TABLE reports.original (i integer)", &[]).unwrap();
    let full = capture(&source, &repository, None);
    source.sql("ALTER DEFAULT PRIVILEGES IN SCHEMA reports REVOKE SELECT ON TABLES FROM report_reader; ALTER DEFAULT PRIVILEGES IN SCHEMA reports GRANT UPDATE ON TABLES TO report_reader WITH GRANT OPTION", &[]).unwrap();
    let incremental = capture(&source, &repository, Some(full.backup()));
    drop(source);
    for (i, (root, can_select)) in [(full, true), (incremental, false)].into_iter().enumerate() {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut job = Engine::new()
            .begin_restore(
                &repository,
                root.backup(),
                destination(provider, &directory.path().join(format!("restored-{i}"))),
                RestoreOptions::new(32 << 20, 8 << 20),
                &control,
            )
            .unwrap();
        prepare(&mut job);
        let restored = job.publish().unwrap();
        restored
            .sql("CREATE TABLE reports.new_report (i integer)", &[])
            .unwrap();
        let row = &restored.sql("SELECT has_table_privilege('report_reader','reports.original','SELECT') AS original, has_table_privilege('report_reader','reports.new_report','SELECT') AS created, has_table_privilege('report_reader','reports.new_report','UPDATE WITH GRANT OPTION') AS delegate", &[]).unwrap().rows[0];
        assert_eq!(row["original"], Value::Bool(true));
        assert_eq!(row["created"], Value::Bool(can_select));
        assert_eq!(row["delegate"], Value::Bool(!can_select));
        restored
            .sql("DROP OWNED BY report_reader; DROP ROLE report_reader", &[])
            .unwrap();
        assert_eq!(
            restored
                .sql("SELECT count(*) AS defaults FROM pg_default_acl", &[])
                .unwrap()
                .rows[0]["defaults"],
            Value::Int(0)
        );
        drop((restored, job));
        assert_eq!(control.memory().used(), 0);
    }
}
