//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::relation_lock_support::{reopen, sessions, sql};
use crate::Engine;
use uqa_core::Value;

fn names(engine: &Engine, target: &str, domain: bool) -> Vec<String> {
    if domain {
        let identity = uqa_core::RelationIdentity::from_legacy_name(target).unwrap();
        let registry = engine.durable.domains.read();
        let definition = &registry[&identity.qualified_name()].definition;
        let mut names = definition
            .not_null
            .iter()
            .filter_map(|constraint| constraint.name.clone())
            .chain(
                definition
                    .checks
                    .iter()
                    .filter_map(|constraint| constraint.name.clone()),
            )
            .collect::<Vec<_>>();
        names.sort();
        return names;
    }
    sql(
        engine,
        &format!(
            "SELECT conname FROM pg_constraint WHERE conrelid='{target}'::regclass ORDER BY conname"
        ),
    )
    .rows
    .into_iter()
    .map(|row| match &row["conname"] {
        Value::Str(name) => name.clone(),
        other => panic!("constraint name: {other:?}"),
    })
    .collect()
}

#[test]
fn automatic_names_avoid_schema_constraints_but_explicit_names_remain_local() {
    for provider in 0..3 {
        let (directory, engine, peer) = sessions(provider);
        drop(peer);
        sql(&engine, "CREATE SCHEMA n; CREATE SCHEMA elsewhere; CREATE TABLE n.blocker(x int CONSTRAINT target_v_not_null CHECK(x>0) CONSTRAINT target_v_check CHECK(x<100) CONSTRAINT target_v_key CHECK(x>1) CONSTRAINT target_pkey CHECK(x>2)); CREATE DOMAIN n.blocked AS int CONSTRAINT target_f_fkey CHECK(VALUE>0); CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE n.foreign_blocker(v int CONSTRAINT target_w_check CHECK(v>0)) SERVER source; CREATE FUNCTION n.fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER target_z_not_null AFTER INSERT ON n.blocker FOR EACH ROW EXECUTE FUNCTION n.fire(); CREATE TABLE n.parent(v int PRIMARY KEY)");
        sql(&engine, "CREATE TABLE n.target(v int NOT NULL CHECK(v>0) UNIQUE, id int PRIMARY KEY, f int REFERENCES n.parent(v), w int CHECK(w>0), z int NOT NULL); CREATE TABLE n.explicit(v int CONSTRAINT target_v_check CHECK(v>0)); CREATE TABLE elsewhere.target(v int NOT NULL CHECK(v>0) UNIQUE, id int PRIMARY KEY)");
        let expected = [
            "target_f_fkey1",
            "target_id_not_null",
            "target_pkey1",
            "target_v_check1",
            "target_v_key1",
            "target_v_not_null1",
            "target_w_check1",
            "target_z_not_null1",
        ];
        assert_eq!(names(&engine, "n.target", false), expected);
        assert_eq!(names(&engine, "n.explicit", false), ["target_v_check"]);
        assert_eq!(
            names(&engine, "elsewhere.target", false),
            [
                "target_id_not_null",
                "target_pkey",
                "target_v_check",
                "target_v_key",
                "target_v_not_null"
            ]
        );
        drop(engine);
        let engine = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(names(&engine, "n.target", false), expected);
    }
}

#[test]
fn domain_and_foreign_declarations_use_the_same_schema_name_scope() {
    for provider in 0..3 {
        let (directory, engine, peer) = sessions(provider);
        drop(peer);
        sql(&engine, "CREATE DOMAIN blockers AS int CONSTRAINT d_not_null CHECK(VALUE>0) CONSTRAINT d_check CHECK(VALUE>0); CREATE TABLE blocked(v int CONSTRAINT f_v_not_null CHECK(v>0) CONSTRAINT f_v_check CHECK(v>0)); CREATE DOMAIN d AS int NOT NULL CHECK(VALUE>0); CREATE DOMAIN explicit AS int CONSTRAINT d_check CHECK(VALUE>0); CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE f(v int NOT NULL CHECK(v>0)) SERVER source");
        assert_eq!(names(&engine, "d", true), ["d_check1", "d_not_null1"]);
        assert_eq!(names(&engine, "explicit", true), ["d_check"]);
        assert_eq!(names(&engine, "f", false), ["f_v_check1", "f_v_not_null1"]);
        drop(engine);
        let engine = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(names(&engine, "d", true), ["d_check1", "d_not_null1"]);
        assert_eq!(names(&engine, "f", false), ["f_v_check1", "f_v_not_null1"]);
    }
}

#[test]
fn recursive_constraint_names_are_chosen_at_the_parent_before_propagation() {
    for provider in 0..3 {
        for alteration in [
            "ALTER TABLE p ALTER COLUMN v SET NOT NULL",
            "ALTER TABLE p ADD NOT NULL v",
        ] {
            let (_directory, engine, _peer) = sessions(provider);
            sql(&engine, "CREATE TABLE p(v int); CREATE TABLE c() INHERITS(p); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER p_v_not_null AFTER INSERT ON c FOR EACH ROW EXECUTE FUNCTION fire(); CREATE TABLE blocked(v int CONSTRAINT p_x_not_null CHECK(v>0) CONSTRAINT p_v_check CHECK(v>0))");
            sql(&engine, alteration);
            sql(
                &engine,
                "ALTER TABLE p ADD COLUMN x int NOT NULL; ALTER TABLE p ADD CHECK(v>0)",
            );
            assert_eq!(
                names(&engine, "p", false),
                ["p_v_check1", "p_v_not_null1", "p_x_not_null1"]
            );
            assert_eq!(
                names(&engine, "c", false),
                [
                    "p_v_check1",
                    "p_v_not_null",
                    "p_v_not_null1",
                    "p_x_not_null1"
                ]
            );
        }
    }
}

#[test]
fn inherited_and_locally_declared_not_null_names_keep_their_distinct_origins() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE TABLE p(v int CONSTRAINT parent_nn NOT NULL); CREATE TABLE inherited() INHERITS(p); CREATE TABLE local(v int NOT NULL) INHERITS(p); CREATE TABLE named(v int CONSTRAINT child_nn NOT NULL) INHERITS(p); CREATE TABLE keyed(v int PRIMARY KEY) INHERITS(p); CREATE TABLE p2(v int CONSTRAINT other_nn NOT NULL); CREATE TABLE both_parents() INHERITS(p,p2)");
        assert_eq!(names(&engine, "inherited", false), ["parent_nn"]);
        assert_eq!(names(&engine, "local", false), ["local_v_not_null"]);
        assert_eq!(names(&engine, "named", false), ["child_nn"]);
        assert_eq!(
            names(&engine, "keyed", false),
            ["keyed_pkey", "keyed_v_not_null"]
        );
        assert_eq!(names(&engine, "both_parents", false), ["parent_nn"]);
    }
}

#[test]
fn automatic_names_see_peer_constraints_while_ordinary_data_remains_pinned() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (directory, first, second) = sessions(provider);
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT changes"),
            );
            sql(&second, "CREATE TABLE blocker(v int CONSTRAINT added_v_check CHECK(v>0)); INSERT INTO t VALUES(2)");
            sql(&first, "CREATE TABLE added(v int CHECK(v>0))");
            assert_eq!(names(&first, "added", false), ["added_v_check1"]);
            assert_eq!(
                sql(&first, "SELECT count(*) AS n FROM t").rows[0]["n"],
                Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
            );
            sql(
                &first,
                "ROLLBACK TO changes; CREATE TABLE added(v int CHECK(v>0)); COMMIT",
            );
            drop((first, second));
            let engine = reopen(provider, &directory.path().join("table-locks.db"));
            assert_eq!(names(&engine, "added", false), ["added_v_check1"]);
        }
    }
}
