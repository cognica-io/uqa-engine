//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::{
    catalog::range_type::{StoredMultirange, StoredRange},
    ColumnType,
};

fn range() -> StoredRange {
    StoredRange {
        object_id: [1; 16],
        oid: 60_001,
        array_oid: 60_002,
        array_name: "_retained_range".into(),
        identity: RelationIdentity::new("public", "retained_range"),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        multirange: StoredMultirange {
            object_id: [2; 16],
            oid: 60_003,
            array_oid: 60_004,
            array_name: "_retained_multi".into(),
            identity: RelationIdentity::new("public", "retained_multi"),
        },
        usage_acl: None,
    }
}

pub(super) fn fixture(engine: &Engine) {
    sql(engine, "CREATE DOMAIN retained_domain AS text DEFAULT 'original' CONSTRAINT nonempty CHECK(VALUE <> ''); CREATE TYPE retained_enum AS ENUM('first','last'); ALTER TYPE retained_enum ADD VALUE 'middle' BEFORE 'last'; CREATE TYPE retained_composite AS (removed integer, kept text); ALTER TYPE retained_composite DROP ATTRIBUTE removed; CREATE TYPE retained_shell");
    engine
        .durable
        .ranges
        .write()
        .insert("public.retained_range".into(), range())
        .unwrap();
}

#[test]
fn memory_type_capture_preserves_definitions_through_rollback_replacement_and_closure() {
    for mutate in [false, true] {
        let engine = Engine::new();
        fixture(&engine);
        let domains = engine.durable.domains.snapshot();
        let enums = engine.durable.enums.snapshot();
        let composites = engine.durable.composites.snapshot();
        let ranges = engine.durable.ranges.snapshot();
        let shells = engine.durable.shells.snapshot();
        let domain_key = engine.durable.domains.key().unwrap();
        let enum_key = engine.durable.enums.key().unwrap();
        let composite_key = engine.durable.composites.key().unwrap();
        let range_key = engine.durable.ranges.key().unwrap();
        let shell_key = engine.durable.shells.key().unwrap();
        let control = StorageReadControl::with_limit(2 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine, "BEGIN; ALTER DOMAIN retained_domain ADD CONSTRAINT changed CHECK(VALUE <> 'private') NOT VALID; ALTER TYPE retained_composite ADD ATTRIBUTE private integer; SAVEPOINT changed; DROP TYPE retained_shell");
            engine.durable.ranges.write().clear();
            sql(&engine, "ROLLBACK TO changed; ROLLBACK");
            assert_eq!(
                engine.durable.ranges.read()["public.retained_range"].object_id,
                [1; 16]
            );
            sql(&engine, "DROP DOMAIN retained_domain; CREATE DOMAIN retained_domain AS integer; DROP TYPE retained_enum; CREATE TYPE retained_enum AS ENUM('replacement'); DROP TYPE retained_composite; CREATE TYPE retained_composite AS (replacement boolean); DROP TYPE retained_shell; CREATE TYPE retained_shell");
            let mut replacement = range();
            replacement.object_id = [3; 16];
            replacement.oid = 60_005;
            engine
                .durable
                .ranges
                .write()
                .insert("public.retained_range".into(), replacement)
                .unwrap();
        }
        drop(engine);
        macro_rules! same {
            ($key:expr, $original:expr) => {{
                let saved = $key.retained(&cut).unwrap().unwrap();
                assert_eq!(
                    serde_json::to_value(saved.get().unwrap()).unwrap(),
                    serde_json::to_value($original.iter().collect::<Vec<_>>()).unwrap()
                );
                saved
            }};
        }
        let saved_domains = same!(domain_key, domains);
        let saved_enums = same!(enum_key, enums);
        let saved_composites = same!(composite_key, composites);
        let saved_ranges = same!(range_key, ranges);
        let saved_shells = same!(shell_key, shells);
        drop(cut);
        assert_eq!(saved_domains.get().unwrap().len(), 1);
        control.cancellation().cancel();
        assert!(saved_domains.get().is_err());
        assert!(saved_enums.get().is_err());
        assert!(saved_composites.get().is_err());
        assert!(saved_ranges.get().is_err());
        assert!(saved_shells.get().is_err());
        drop((
            saved_domains,
            saved_enums,
            saved_composites,
            saved_ranges,
            saved_shells,
        ));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn denied_type_capture_keeps_type_ddl_and_independent_cuts_usable() {
    let engine = Engine::new();
    sql(
        &engine,
        &format!(
            "CREATE DOMAIN retained_domain AS text DEFAULT '{}'",
            "x".repeat(16384)
        ),
    );
    let original = engine.durable.domains.snapshot();
    let key = engine.durable.domains.key().unwrap();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let denied = registry.capture(&small).unwrap();
    let saved = registry.capture(&good).unwrap();
    sql(
        &engine,
        "ALTER DOMAIN retained_domain ADD CONSTRAINT changed CHECK(VALUE <> 'forbidden') NOT VALID",
    );
    assert!(denied.check().is_err());
    let captured = key.retained(&saved).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(captured.get().unwrap()).unwrap(),
        serde_json::to_value(original.iter().collect::<Vec<_>>()).unwrap()
    );
    sql(
        &engine,
        "DROP DOMAIN retained_domain; CREATE DOMAIN retained_domain AS integer",
    );
    drop((captured, saved, denied, engine));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
