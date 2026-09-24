//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn alteration_lock_modes_follow_postgresql_action_requirements() {
    for (sql, expected) in [
        (
            "ALTER TABLE t DISABLE TRIGGER USER",
            TableLockMode::ShareRowExclusive,
        ),
        (
            "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id) NOT VALID",
            TableLockMode::ShareRowExclusive,
        ),
        (
            "ALTER TABLE t VALIDATE CONSTRAINT positive",
            TableLockMode::ShareUpdateExclusive,
        ),
        (
            "ALTER TABLE p ATTACH PARTITION t FOR VALUES FROM(0) TO(10)",
            TableLockMode::ShareUpdateExclusive,
        ),
        (
            "ALTER TABLE p DETACH PARTITION t CONCURRENTLY",
            TableLockMode::ShareUpdateExclusive,
        ),
        (
            "ALTER TABLE p DETACH PARTITION t FINALIZE",
            TableLockMode::ShareUpdateExclusive,
        ),
        (
            "ALTER TABLE p DETACH PARTITION t",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER TABLE t ADD COLUMN extra integer",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER TABLE t OWNER TO reader",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER TABLE t RENAME TO renamed",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER TABLE t DISABLE RULE r",
            TableLockMode::AccessExclusive,
        ),
    ] {
        let crate::Statement::AlterTable(statement) = crate::compile(sql).unwrap().remove(0) else {
            panic!("expected ALTER TABLE: {sql}");
        };
        assert_eq!(table_alter_lock_mode(&statement), expected, "{sql}");
    }
}

#[test]
fn combined_alterations_use_the_strongest_mode_in_either_order() {
    for (actions, expected) in [
        (
            ["DISABLE TRIGGER USER", "ADD COLUMN extra integer"],
            TableLockMode::AccessExclusive,
        ),
        (
            ["VALIDATE CONSTRAINT positive", "DISABLE TRIGGER USER"],
            TableLockMode::ShareRowExclusive,
        ),
    ] {
        for ordered in [actions, [actions[1], actions[0]]] {
            let sql = format!("ALTER TABLE t {}", ordered.join(", "));
            let crate::Statement::AlterTable(statement) = crate::compile(&sql).unwrap().remove(0)
            else {
                panic!("expected ALTER TABLE: {sql}");
            };
            assert_eq!(table_alter_lock_mode(&statement), expected, "{sql}");
        }
    }
}
