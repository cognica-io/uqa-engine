//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use std::collections::{BTreeMap, BTreeSet};
use uqa_execution::catalog::comments::CommentAddress;
use uqa_sql::{ast::RelationPersistence, catalog::dependencies::*};

mod native;

fn addresses(engine: &Engine, class_id: u32, query: &str) -> Vec<CommentAddress> {
    let result = engine.sql(query, &[]).unwrap();
    assert!(!result.rows.is_empty(), "{query}");
    result
        .rows
        .iter()
        .map(|row| {
            let Value::Int(oid) = row["oid"] else {
                panic!("catalog OID")
            };
            ObjectAddress {
                class_id,
                object_id: u32::try_from(oid).unwrap(),
                sub_id: 0,
            }
            .into()
        })
        .collect()
}

fn definitions(engine: &Engine) -> BTreeSet<CommentAddress> {
    sql(engine, "CREATE TABLE docs(id integer); INSERT INTO docs VALUES(1); CREATE TYPE saved_type AS ENUM('saved'); COMMENT ON TYPE saved_type IS 'original 한글'; CREATE UNLOGGED TABLE unlogged_docs(id integer); CREATE TEMP TABLE temp_docs(id integer CONSTRAINT positive CHECK(id > 0) DEFAULT 1, CONSTRAINT temp_key UNIQUE(id)); CREATE INDEX temp_index ON temp_docs(id); CREATE TEMP VIEW temp_view AS SELECT id FROM temp_docs; CREATE TEMP SEQUENCE temp_seq; CREATE DOMAIN pg_temp.temp_domain AS integer CONSTRAINT domain_positive CHECK(VALUE > 0); CREATE TYPE pg_temp.temp_enum AS ENUM('one','two'); CREATE TYPE pg_temp.temp_composite AS (v integer); CREATE TYPE pg_temp.temp_shell; CREATE FUNCTION pg_temp.temp_function() RETURNS integer LANGUAGE SQL RETURN 1; CREATE FUNCTION pg_temp.fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER temp_event AFTER INSERT ON temp_docs FOR EACH ROW EXECUTE FUNCTION pg_temp.fire(); CREATE RULE temp_rule AS ON UPDATE TO temp_docs DO ALSO NOTHING; CREATE TEMP TABLE temp_parent(id integer) PARTITION BY RANGE(id); CREATE TEMP TABLE temp_child PARTITION OF temp_parent FOR VALUES FROM(0) TO(10); CREATE TRIGGER inherited BEFORE INSERT ON temp_parent FOR EACH ROW EXECUTE FUNCTION pg_temp.fire()");
    let namespace = engine.temporary_namespace_oids().unwrap();
    let namespace_oid = namespace.namespace;
    let queries = [
        (RELATION_CLASS, format!("SELECT oid FROM pg_class WHERE relnamespace={namespace_oid}")),
        (TYPE_CLASS, format!("SELECT oid FROM pg_type WHERE typnamespace={namespace_oid}")),
        (PROCEDURE_CLASS, format!("SELECT oid FROM pg_proc WHERE pronamespace={namespace_oid}")),
        (CONSTRAINT_CLASS, format!("SELECT oid FROM pg_constraint WHERE connamespace={namespace_oid}")),
        (ATTRIBUTE_DEFAULT_CLASS, format!("SELECT a.oid FROM pg_attrdef a JOIN pg_class c ON c.oid=a.adrelid WHERE c.relnamespace={namespace_oid}")),
        (TRIGGER_CLASS, format!("SELECT t.oid FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid WHERE c.relnamespace={namespace_oid}")),
        (REWRITE_CLASS, format!("SELECT r.oid FROM pg_rewrite r JOIN pg_class c ON c.oid=r.ev_class WHERE c.relnamespace={namespace_oid}")),
        (uqa_sql::schema::constraint_metadata::CatalogOidClass::EnumLabel.class_id(), format!("SELECT e.oid FROM pg_enum e JOIN pg_type t ON t.oid=e.enumtypid WHERE t.typnamespace={namespace_oid}")),
        (NAMESPACE_CLASS, format!("SELECT oid FROM pg_namespace WHERE oid={} OR oid={}", namespace.namespace, namespace.toast_namespace)),
    ];
    let mut excluded = BTreeSet::new();
    for (class, query) in queries {
        excluded.extend(addresses(engine, class, &query));
    }
    let temp_relation = engine
        .table("temp_docs")
        .unwrap()
        .unwrap()
        .recorded_catalog_oids()
        .unwrap()
        .relation;
    excluded.insert(
        ObjectAddress {
            class_id: RELATION_CLASS,
            object_id: temp_relation,
            sub_id: 1,
        }
        .into(),
    );
    let mut comments = engine.durable.comments.write();
    for address in &excluded {
        comments
            .insert(*address, "temporary", RelationPersistence::Temporary)
            .unwrap();
    }
    comments
        .insert(
            ObjectAddress {
                class_id: 999,
                object_id: temp_relation,
                sub_id: 1,
            }
            .into(),
            "opaque class",
            RelationPersistence::Permanent,
        )
        .unwrap();
    comments
        .insert(
            ObjectAddress {
                class_id: RELATION_CLASS,
                object_id: u32::MAX - 1,
                sub_id: 7,
            }
            .into(),
            "dangling",
            RelationPersistence::Permanent,
        )
        .unwrap();
    excluded
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_comment_sources_preserve_original_object_eligibility_after_closure(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    let excluded = definitions(&engine);
    let original = engine.durable.comments.snapshot();
    let weak = Arc::downgrade(&original);
    let expected = original
        .iter()
        .filter(|(address, _)| !excluded.contains(address))
        .map(|(address, text)| (*address, text.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(expected.len(), 3);
    drop(original);
    sql(&engine, "BEGIN; COMMENT ON TYPE saved_type IS 'private'; DROP TYPE pg_temp.temp_enum; CREATE TYPE pg_temp.temp_enum AS ENUM('replacement'); COMMENT ON TYPE pg_temp.temp_enum IS 'replacement'");
    let control = StorageReadControl::with_limit(64 << 10);
    let selected = capture(&engine, &control);
    sql(&engine, completion);
    sql(&engine, "DROP TYPE saved_type; CREATE TYPE saved_type AS ENUM('new'); COMMENT ON TYPE saved_type IS 'new generation'");
    drop(engine);
    assert!(weak.upgrade().is_none());
    let captured = selected.comments.captured(&selected.cut).unwrap();
    let saved = captured
        .get()
        .unwrap()
        .iter()
        .map(|(address, text)| (*address, text.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(saved, expected);
    assert!(saved.keys().all(|address| !excluded.contains(address)));
    drop(selected);
    control.cancellation().cancel();
    assert!(captured.get().is_err());
    drop(captured);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn committed_comment_roots_capture_original_text_with_bounded_work() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        for index in 0..count {
            sql(&engine, &format!("CREATE TYPE saved_{index:03} AS ENUM ('one'); COMMENT ON TYPE saved_{index:03} IS 'original 한글'"));
        }
        let original = engine.durable.comments.snapshot();
        let weak = Arc::downgrade(&original);
        let expected = original
            .iter()
            .map(|(address, text)| (*address, text.clone()))
            .collect::<BTreeMap<_, _>>();
        drop(original);
        sql(&engine, "BEGIN; COMMENT ON TYPE saved_000 IS 'private'; SAVEPOINT nested; COMMENT ON TYPE saved_000 IS 'nested'; ROLLBACK TO nested");
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut retained = None;
        let allocated = allocation_counter::measure(|| {
            retained = Some(selected.comments.captured(&selected.cut).unwrap());
        });
        assert!(
            allocated.bytes_total < 16 << 10 && allocated.count_total < 64,
            "count={count}: {allocated:?}"
        );
        let retained = retained.unwrap();
        sql(&engine, "COMMIT; DROP TYPE saved_000; CREATE TYPE saved_000 AS ENUM ('replacement'); COMMENT ON TYPE saved_000 IS 'replacement'");
        drop(engine);
        assert!(weak.upgrade().is_none());
        assert!(retained.get().unwrap().iter().eq(expected.iter()));
        drop(selected);
        control.cancellation().cancel();
        assert!(retained.get().is_err());
        drop(retained);
        assert_eq!(control.memory().used(), 0);
    }
}
