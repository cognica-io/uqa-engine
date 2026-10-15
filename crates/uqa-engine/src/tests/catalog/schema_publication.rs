//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unchanged constraint modes must not replace retained table-definition metadata.

use crate::tests::relation_lock_support::{sessions, sql};

fn revision(engine: &crate::Engine, name: &str) -> Option<String> {
    let table = engine.try_table(name).unwrap().unwrap();
    let key = uqa_execution::catalog::definition_revision::table_key(table.object_id());
    engine
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .get_metadata(&key)
        .unwrap()
}

#[test]
fn repeated_constraint_modes_preserve_durable_definition_revisions() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE TABLE p(id integer PRIMARY KEY); CREATE TABLE c(id integer CONSTRAINT n NOT NULL, ref integer, CONSTRAINT f FOREIGN KEY(ref) REFERENCES p(id))");
        for (statement, changed) in [
            ("ALTER TABLE c ALTER CONSTRAINT f NOT DEFERRABLE", false),
            ("ALTER TABLE c ALTER CONSTRAINT f DEFERRABLE", true),
            ("ALTER TABLE c ALTER CONSTRAINT f DEFERRABLE", false),
            ("ALTER TABLE c ALTER CONSTRAINT f INITIALLY DEFERRED", true),
            ("ALTER TABLE c ALTER CONSTRAINT f INITIALLY DEFERRED", false),
            ("ALTER TABLE c ALTER CONSTRAINT f NOT ENFORCED", true),
            ("ALTER TABLE c ALTER CONSTRAINT f NOT ENFORCED", false),
            ("ALTER TABLE c ALTER CONSTRAINT f ENFORCED", true),
            ("ALTER TABLE c ALTER CONSTRAINT f ENFORCED", false),
            ("ALTER TABLE c ALTER CONSTRAINT n INHERIT", false),
            ("ALTER TABLE c ALTER CONSTRAINT n NO INHERIT", true),
            ("ALTER TABLE c ALTER CONSTRAINT n NO INHERIT", false),
            ("ALTER TABLE c ALTER CONSTRAINT n INHERIT", true),
        ] {
            let before = revision(&engine, "public.c");
            sql(&engine, statement);
            assert_eq!(
                before != revision(&engine, "public.c"),
                changed,
                "provider {provider}, {statement}",
            );
        }
        sql(&engine, "CREATE TABLE partitioned_c(id integer, ref integer, CONSTRAINT pf FOREIGN KEY(ref) REFERENCES p(id)) PARTITION BY RANGE(id); CREATE TABLE child_c PARTITION OF partitioned_c FOR VALUES FROM (MINVALUE) TO (MAXVALUE)");
        for (statement, changed) in [
            (
                "ALTER TABLE partitioned_c ALTER CONSTRAINT pf NOT DEFERRABLE",
                false,
            ),
            (
                "ALTER TABLE partitioned_c ALTER CONSTRAINT pf DEFERRABLE",
                true,
            ),
            (
                "ALTER TABLE partitioned_c ALTER CONSTRAINT pf DEFERRABLE",
                false,
            ),
        ] {
            let parent_before = revision(&engine, "public.partitioned_c");
            let child_before = revision(&engine, "public.child_c");
            sql(&engine, statement);
            assert_eq!(
                parent_before != revision(&engine, "public.partitioned_c"),
                changed
            );
            assert_eq!(child_before != revision(&engine, "public.child_c"), changed);
        }
    }
}
