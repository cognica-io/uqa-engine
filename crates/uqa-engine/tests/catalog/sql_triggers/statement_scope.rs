//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement fires the row triggers its table had when the statement began, and reads the replication role at each firing. The expected rows are those `PostgreSQL` 18.4 returns for the same statements.

use super::*;

/// A table whose row triggers log the rows they see through `later()`.
fn fixture() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id integer);
         CREATE TABLE log (msg text);
         CREATE FUNCTION later() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO log VALUES ('later ' || NEW.id);
           RETURN NEW;
         END
         $$",
    );
    engine
}

fn logged(engine: &Engine) -> Vec<String> {
    strings(engine, "SELECT msg FROM log ORDER BY msg", "msg")
}

#[test]
fn a_trigger_created_while_a_statement_runs_fires_from_the_next_statement() {
    let engine = fixture();
    exec(
        &engine,
        "CREATE FUNCTION first() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.id = 1 THEN
             CREATE TRIGGER later_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION later();
           END IF;
           RETURN NEW;
         END
         $$;
         CREATE TRIGGER a_first BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION first()",
    );
    exec(&engine, "INSERT INTO t VALUES (1), (2), (3)");
    assert_eq!(logged(&engine), Vec::<String>::new());
    exec(&engine, "INSERT INTO t VALUES (4)");
    assert_eq!(logged(&engine), ["later 4"]);
}

#[test]
fn a_trigger_dropped_while_a_statement_runs_fires_for_the_rest_of_the_statement() {
    let engine = fixture();
    exec(
        &engine,
        "CREATE FUNCTION dropper() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.id = 10 THEN
             DROP TRIGGER later_trigger ON t;
           END IF;
           RETURN NEW;
         END
         $$;
         CREATE TRIGGER a_dropper BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION dropper();
         CREATE TRIGGER later_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION later()",
    );
    exec(&engine, "INSERT INTO t VALUES (10), (11), (12)");
    assert_eq!(logged(&engine), ["later 10", "later 11", "later 12"]);
    exec(&engine, "INSERT INTO t VALUES (13)");
    assert_eq!(logged(&engine), ["later 10", "later 11", "later 12"]);
}

#[test]
fn the_replication_role_is_read_at_each_firing() {
    let engine = fixture();
    exec(
        &engine,
        "CREATE FUNCTION replica() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.id = 30 THEN
             SET session_replication_role = replica;
           END IF;
           RETURN NEW;
         END
         $$;
         CREATE TRIGGER a_replica BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION replica();
         ALTER TABLE t ENABLE ALWAYS TRIGGER a_replica;
         CREATE TRIGGER z_noisy BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION later();
         CREATE TRIGGER z_after AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION later()",
    );
    // The trigger that sorts first changes the role, and no trigger that fires only in the origin role follows it, for its own row or for a later one.
    exec(&engine, "INSERT INTO t VALUES (30), (31), (32)");
    assert_eq!(logged(&engine), Vec::<String>::new());
    assert_eq!(
        strings(
            &engine,
            "SHOW session_replication_role",
            "session_replication_role"
        ),
        ["replica"]
    );
    exec(
        &engine,
        "RESET session_replication_role; INSERT INTO t VALUES (33)",
    );
    assert_eq!(logged(&engine), ["later 33", "later 33"]);
}
