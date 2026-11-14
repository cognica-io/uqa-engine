//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, Value,
};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn retained_session_namespace_survives_setting_role_and_source_retirement(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("names"));
    source.sql("CREATE ROLE selected; CREATE SCHEMA selected; CREATE TEMP TABLE local_item(id integer); SET ROLE selected; SET search_path = \"$user\", public, public", &[]).unwrap();
    let mut selected = None;
    let capture = allocation_counter::measure(|| {
        selected = Some(source.session.namespace_snapshot(source.session_id));
    });
    assert_eq!(capture.bytes_total, 0);
    let selected = selected.unwrap();
    assert!(selected.temporary_namespace_allocated);
    let role = selected.role.clone();
    let path = selected.search_path.clone();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let retained_native = native.clone();
    let original = token.clone();
    let mut input = ImageDecoder::<_, SQLError>::new(&work, &token, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&retained_native, &original, input.cancellation);
        let temporary_schema = control.format(format_args!("pg_temp_{}", selected.session_id))?;
        RelationNameResolution::from_inputs_with_work(
            selected.expanded_path(),
            temporary_schema,
            selected.temporary_namespace_allocated,
            &RoleReference::Bound(selected.role.clone()),
            RelationLookupMode::Dynamic,
            &control,
            &mut input,
        )
        .await
    })
    .unwrap();
    let window = ImageDecodeWindow { bytes: 1, steps: 1 };
    assert!(input
        .advance(&mut std::io::empty(), window)
        .unwrap()
        .is_none());
    source.sql("RESET ROLE; ALTER ROLE selected RENAME TO later; SET search_path = public; DISCARD TEMP", &[]).unwrap();
    drop(source);
    let retained = input.read(&mut std::io::empty()).unwrap();
    assert_eq!(*path, ["$user", "public", "public"]);
    assert_eq!(retained.search_path, ["selected", "public", "public"]);
    assert!(retained.temporary_namespace_allocated);
    assert_eq!(retained.lookup_mode, RelationLookupMode::Dynamic);
    let RoleReference::Bound(bound) = &retained.current_user else {
        panic!("role identity was copied")
    };
    assert!(Arc::ptr_eq(&role, bound));
    assert_eq!(work.used(), 0);
    assert_eq!(native.used(), retained.reserved_bytes());
    drop(retained);
    assert_eq!(native.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn retained_search_path_generation_preserves_transaction_and_routine_scope(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("settings"));
    source.sql("CREATE SCHEMA first_path; CREATE SCHEMA second_path; SET search_path = first_path, public", &[]).unwrap();
    let original = source.session.namespace_snapshot(source.session_id);
    source
        .sql(
            "BEGIN; SAVEPOINT setting; SET LOCAL search_path = second_path, public",
            &[],
        )
        .unwrap();
    let local = source.session.namespace_snapshot(source.session_id);
    assert_eq!(*local.search_path, ["second_path", "public"]);
    source
        .sql(
            "ROLLBACK TO SAVEPOINT setting; RELEASE SAVEPOINT setting",
            &[],
        )
        .unwrap();
    let restored = source.session.namespace_snapshot(source.session_id);
    assert!(Arc::ptr_eq(&original.search_path, &restored.search_path));
    source
        .sql("SET search_path = second_path; ROLLBACK", &[])
        .unwrap();
    assert!(Arc::ptr_eq(
        &original.search_path,
        &source.session.state.read().search_path
    ));
    source.sql("CREATE FUNCTION public.saved_path() RETURNS text LANGUAGE SQL SET search_path = second_path, public AS $$ SELECT current_schema() $$", &[]).unwrap();
    let returned = source
        .sql("SELECT public.saved_path() AS name", &[])
        .unwrap();
    assert_eq!(returned.rows[0]["name"], Value::Str("second_path".into()));
    assert_eq!(source.search_path(), ["first_path", "public"]);
    assert_eq!(*original.search_path, ["first_path", "public"]);
    source.sql("RESET search_path", &[]).unwrap();
    assert_eq!(source.search_path(), ["$user", "public"]);
    source.sql("SET search_path = second_path", &[]).unwrap();
    source.sql("DISCARD ALL", &[]).unwrap();
    assert_eq!(source.search_path(), ["$user", "public"]);
    assert_eq!(*local.search_path, ["second_path", "public"]);
}
