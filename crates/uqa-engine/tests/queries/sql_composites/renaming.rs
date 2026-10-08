//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_attribute_renames_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("composite-rename.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_attribute_rename_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        super::addition::open(provider, &path)
    };
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn legacy_catalog_and_peer_renames_preserve_prepared_definition_identity(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("composite-rename-peer.db");
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_attribute_rename_oracle.expected.json"
    ))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap().clone();
    let case = |name: &str| {
        cases
            .iter()
            .find(|case| case["id"] == name)
            .unwrap()
            .clone()
    };
    let verify = |engine: &uqa_engine::Engine, reference: &mut serde_json::Value, case| {
        reference["cases"] = serde_json::json!([case]);
        crate::pg18_oracle::verify(engine, &reference.to_string());
    };
    let engine = super::addition::open(provider, &path);
    verify(&engine, &mut reference, case("check_cache_setup"));
    drop(engine);
    // Existing catalogs have no analysis markers. Strip only those records from this disposable fixture.
    super::addition::restoration::catalog(provider, &path, |catalog| {
        for (key, _) in catalog.metadata_with_prefix("__uqa_prepared_").unwrap() {
            catalog.delete_metadata(&key).unwrap();
        }
    });
    let engine = super::addition::open(provider, &path);
    let peer = engine.new_session().unwrap();
    verify(&engine, &mut reference, case("check_cache_prepare"));
    peer.sql("ALTER TYPE cr_check_pre RENAME ATTRIBUTE b TO label", &[])
        .unwrap();
    let mut renamed = case("check_cache_rename");
    renamed["sql"] = "EXECUTE cr_check_q; EXECUTE cr_check_vq".into();
    renamed["command_tags"].as_array_mut().unwrap().remove(0);
    renamed["results"].as_array_mut().unwrap().remove(0);
    verify(&engine, &mut reference, renamed);
    verify(&engine, &mut reference, case("check_cache_rollback"));
    verify(&engine, &mut reference, case("check_cache_same_definition"));
    verify(
        &engine,
        &mut reference,
        case("check_cache_definition_change"),
    );
    engine.sql("DROP TABLE cr_check_t CASCADE", &[]).unwrap();
    drop(peer);
    drop(engine);
    super::addition::restoration::catalog(provider, &path, |catalog| {
        assert!(catalog
            .metadata_with_prefix("__uqa_prepared_")
            .unwrap()
            .is_empty());
    });
}
