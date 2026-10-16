//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range definition adapters preserve private generations, rollback and durable provider identity.

use crate::tests::relation_lock_support::{reopen, sessions, sql};
use crate::Engine;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity, Value};
use uqa_sql::{
    catalog::range_type::{StoredMultirange, StoredRange},
    ColumnType,
};

fn definition() -> StoredRange {
    StoredRange {
        object_id: [71; 16],
        oid: 51_000,
        array_oid: 51_001,
        array_name: "_ranges".into(),
        identity: RelationIdentity::new("public", "ranges"),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1_978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        multirange: StoredMultirange {
            object_id: [72; 16],
            oid: 51_002,
            array_oid: 51_003,
            array_name: "_multiranges".into(),
            identity: RelationIdentity::new("public", "multiranges"),
        },
        usage_acl: None,
    }
}

fn publish(
    engine: &Engine,
    change: impl FnOnce(&mut uqa_execution::catalog::range_type::RangeRegistry),
) {
    engine
        .with_implicit_definition_transaction(|engine| {
            let before = engine.durable.ranges.read().clone();
            let mut after = before.clone();
            change(&mut after);
            uqa_execution::catalog::range_type::publish(engine, &before, after)?;
            engine.note_catalog_registry_changed();
            Ok(())
        })
        .unwrap();
}

#[test]
fn declared_range_generations_survive_provider_refresh_savepoints_and_reopen() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        let definition = definition();
        sql(&first, "BEGIN; SAVEPOINT ranges");
        publish(&first, |registry| {
            registry.insert("public.ranges".into(), definition.clone());
        });
        let pinned = first.catalog_read_view();
        assert_eq!(
            sql(&first, "SELECT typname FROM pg_type WHERE oid=51000").rows[0]["typname"],
            Value::Str("ranges".into())
        );
        assert!(sql(&second, "SELECT typname FROM pg_type WHERE oid=51000")
            .rows
            .is_empty());
        sql(&second, "CREATE TYPE peer AS ENUM('visible')");
        first.list_named_analyzers().unwrap();
        assert!(first.durable.ranges.read().contains_key("public.ranges"));
        sql(&first, "ROLLBACK TO ranges; COMMIT");
        assert!(first
            .catalog_read_view()
            .range_value_descriptor(51_000)
            .unwrap()
            .is_none());
        assert!(pinned.range_value_descriptor(51_000).unwrap().is_some());
        publish(&first, |registry| {
            registry.insert("public.ranges".into(), definition.clone());
        });
        assert_eq!(
            sql(
                &second,
                "SELECT rngsubtype FROM pg_range WHERE rngtypid=51000"
            )
            .rows[0]["rngsubtype"],
            Value::Int(23)
        );
        sql(&first, "BEGIN");
        publish(&first, |registry| {
            registry
                .get_mut("public.ranges")
                .unwrap()
                .multirange
                .identity
                .name = "changed".into();
        });
        sql(&first, "ROLLBACK");
        drop(pinned);
        drop((first, second));
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        let registry = restored.durable.ranges.read();
        assert_eq!(
            serde_json::to_value(&registry["public.ranges"]).unwrap(),
            serde_json::to_value(&definition).unwrap()
        );
        drop(registry);
        let descriptor = uqa_sql::expr::SQLValueCatalog::value_range_type(&restored, 51_000)
            .unwrap()
            .unwrap();
        assert_eq!(descriptor.endpoint, ColumnType::Integer);
        // An already admitted [1,2] integer range, carrying this catalog's type OID.
        let mut bytes = Vec::from((17_u32 << 2).to_le_bytes());
        bytes.extend_from_slice(&51_000_u32.to_le_bytes());
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        bytes.extend_from_slice(&2_i32.to_le_bytes());
        bytes.push(6);
        let datum = uqa_core::DatumValue::new(51_000, 0, bytes);
        assert_eq!(
            uqa_sql::expr::datums::read_with_catalog(&datum, Some(&restored)).unwrap(),
            Value::Str("[1,2]".into())
        );
        sql(&restored, "SELECT 1::integer");
    }
}
