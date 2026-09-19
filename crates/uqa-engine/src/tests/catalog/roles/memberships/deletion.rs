//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn prepare(first: &Engine, second: &Engine, isolation: &str) {
    sql(first, "CREATE ROLE target; CREATE ROLE member; CREATE ROLE grantor; GRANT target TO grantor WITH ADMIN TRUE, INHERIT FALSE, SET FALSE");
    sql(
        second,
        &format!("BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2)"),
    );
}

#[test]
fn target_and_grantor_deletion_waits_follow_both_command_orders_and_transaction_undo() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for endpoint in ["target", "grantor"] {
                for drop_first in [false, true] {
                    for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                        let (directory, first, second) = sessions(provider);
                        prepare(&first, &second, isolation);
                        let lock = role_lock(&first, endpoint);
                        sql(&first, "BEGIN; SAVEPOINT undo");
                        let drop_statement = format!("DROP ROLE {endpoint}");
                        let grant =
                            "SET LOCAL ROLE grantor; GRANT target TO member WITH INHERIT TRUE";
                        sql(&first, if drop_first { &drop_statement } else { grant });
                        let (second, result) = after_wait(
                            &first,
                            second,
                            if drop_first { grant } else { &drop_statement },
                            lock,
                            finish,
                        );
                        let expected = if finish == "COMMIT" && endpoint == "grantor" {
                            if drop_first {
                                "42704"
                            } else {
                                "2BP01"
                            }
                        } else {
                            "00000"
                        };
                        if expected == "00000" {
                            result.unwrap();
                            sql(&second, "COMMIT");
                        } else {
                            assert_eq!(
                                result.unwrap_err().sqlstate(),
                                Some(expected),
                                "{provider}/{isolation}/{endpoint}/{drop_first}/{finish}"
                            );
                            sql(&second, "ROLLBACK");
                        }
                        let rows = membership_rows(&first);
                        let granted = first
                            .durable
                            .role_memberships
                            .read()
                            .values()
                            .any(|row| row.member.name == "member");
                        assert_eq!(
                            granted,
                            if drop_first {
                                expected == "00000"
                            } else {
                                finish == "COMMIT" && endpoint == "grantor"
                            }
                        );
                        let expected_count = if expected == "00000" { 2 } else { 1 };
                        assert_eq!(sql(&first, "SELECT v FROM t").rows.len(), expected_count);
                        drop(second);
                        drop(first);
                        let reopened = reopen(provider, &directory.path().join("table-locks.db"));
                        assert_eq!(membership_rows(&reopened), rows);
                        assert_eq!(sql(&reopened, "SELECT v FROM t").rows.len(), expected_count);
                    }
                }
            }
        }
    }
}

#[test]
fn member_deletion_has_no_dependency_wait_and_never_rebinds_a_recreated_member() {
    for provider in 0..3 {
        for drop_first in [false, true] {
            let (directory, first, second) = sessions(provider);
            prepare(&first, &second, "REPEATABLE READ");
            let original = first.durable.roles.read()["member"].identity();
            let grant = "SET LOCAL ROLE grantor; GRANT target TO member WITH INHERIT TRUE";
            sql(&first, "BEGIN");
            sql(
                &first,
                if drop_first {
                    "DROP ROLE member"
                } else {
                    grant
                },
            );
            let second = before_holder_ends(
                &first,
                second,
                &format!(
                    "{}; COMMIT",
                    if drop_first {
                        grant
                    } else {
                        "DROP ROLE member"
                    }
                ),
            );
            sql(&first, "COMMIT; CREATE ROLE member");
            assert_ne!(first.durable.roles.read()["member"].identity(), original);
            let memberships = first.durable.role_memberships.read();
            assert!(memberships
                .values()
                .any(|row| row.member.identity() == original));
            drop(memberships);
            assert_eq!(
                sql(
                    &first,
                    "SELECT pg_has_role('member', 'target', 'MEMBER') AS member"
                )
                .rows[0]["member"],
                Value::Bool(false)
            );
            let rows = membership_rows(&first);
            drop(second);
            drop(first);
            let reopened = reopen(provider, &directory.path().join("table-locks.db"));
            assert_eq!(membership_rows(&reopened), rows);
        }
    }
}
