//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::{
    catalog::range_type::{StoredMultirange, StoredRange},
    ColumnType,
};

fn publish(engine: &Engine, index: u8) {
    let oid = 51000 + u32::from(index) * 4;
    let name = format!("range_{index}");
    let multi = format!("multirange_{index}");
    let definition = StoredRange {
        object_id: [index * 2 + 1; 16],
        oid,
        array_oid: oid + 1,
        array_name: format!("_{name}"),
        identity: RelationIdentity::new("public", name),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        multirange: StoredMultirange {
            object_id: [index * 2 + 2; 16],
            oid: oid + 2,
            array_oid: oid + 3,
            array_name: format!("_{multi}"),
            identity: RelationIdentity::new("public", multi),
        },
        usage_acl: None,
    };
    engine
        .with_implicit_definition_transaction(|engine| {
            let before = engine.durable.ranges.read().clone();
            let mut after = before.clone();
            after.insert(definition.identity.qualified_name(), definition)?;
            uqa_execution::catalog::range_type::publish(engine, &before, after)?;
            engine.note_catalog_registry_changed();
            Ok(())
        })
        .unwrap();
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_structured_types_keep_selected_roots_and_later_sql_mutation_ownership(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("structured"));
    source.sql("CREATE TYPE payload AS(id integer, name text); COMMENT ON TYPE payload IS 'original 한글'",&[]).unwrap();
    publish(&source, 0);
    let ranges = serde_json::to_value(&*source.durable.ranges.read()).unwrap();
    let composites = serde_json::to_value(&*source.durable.composites.read()).unwrap();
    let comments = source.durable.comments.read().clone();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_types(&mut input, Kind::Range);
    assert!(input.advance(16).unwrap().is_none());
    source
        .sql(
            "COMMENT ON TYPE payload IS 'peer'; CREATE TYPE later AS(value integer)",
            &[],
        )
        .unwrap();
    publish(&source, 1);
    let session = finish(&mut input);
    assert_eq!(
        serde_json::to_value(&*session.engine().durable.ranges.read()).unwrap(),
        ranges
    );
    assert_eq!(
        serde_json::to_value(&*session.engine().durable.composites.read()).unwrap(),
        composites
    );
    assert_eq!(*session.engine().durable.comments.read(), comments);
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.sql("ALTER TYPE payload ADD ATTRIBUTE extra integer; COMMENT ON TYPE payload IS 'restored'; CREATE TYPE another AS(value integer)",&[]).unwrap();
    publish(&restored, 2);
    assert!(restored
        .durable
        .ranges
        .read()
        .contains_key("public.range_1"));
    assert!(restored
        .durable
        .ranges
        .read()
        .contains_key("public.range_2"));
    assert!(restored
        .durable
        .composites
        .read()
        .contains_key("public.later"));
    assert!(restored
        .durable
        .composites
        .read()
        .contains_key("public.another"));
    assert_eq!(
        restored
            .sql(
                "SELECT obj_description('payload'::regtype::oid,'pg_type') AS description",
                &[]
            )
            .unwrap()
            .rows[0]["description"],
        Value::Str("restored".into())
    );
    drop((restored, input, source, comments));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_structured_types_retire_private_roots_and_selected_transactions(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("structured"));
    for i in 0..16 {
        source
            .sql(
                &format!(
                    "CREATE TYPE c_{i} AS(id integer, name text); COMMENT ON TYPE c_{i} IS '{}'",
                    "x".repeat(16384)
                ),
                &[],
            )
            .unwrap();
        publish(&source, i);
    }
    let catalog = source.storage.catalog.as_ref().unwrap();
    let original = catalog.metadata_with_prefix("").unwrap();
    for kind in [Kind::Range, Kind::Comment, Kind::Composite] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            terminate(&source, kind, terminal);
        }
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), original);
    for marker in ["sql_range_types_json", "sql_composites_json"] {
        let saved = catalog.get_metadata(marker).unwrap().unwrap();
        catalog.set_metadata(marker, "{bad JSON").unwrap();
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        let backend = Arc::clone(
            input
                .candidate
                .as_ref()
                .unwrap()
                .session
                .engine()
                .storage
                .backend
                .as_ref()
                .unwrap(),
        );
        loop {
            match input.advance(64) {
                Err(error) => {
                    assert_marker_error(error, "{bad JSON");
                    break;
                }
                Ok(None) => (),
                Ok(Some(_)) => panic!("corrupt structured type published"),
            }
        }
        assert!(!backend.in_transaction());
        assert!(input.advance(1).is_err());
        assert_eq!(control.memory().used(), 0);
        catalog.set_metadata(marker, &saved).unwrap();
    }
    source
        .sql(
            "CREATE TYPE recovered AS(value integer); COMMENT ON TYPE recovered IS 'ok'",
            &[],
        )
        .unwrap();
    publish(&source, 100);
}
