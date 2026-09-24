//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn target_wait_reads_current_member_defaults_and_retains_original_endpoint_identities(
    #[case] provider: usize,
    #[values(0, 1, 2)] isolation_index: usize,
) {
    let isolation = ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"][isolation_index];
    let (directory, first, mut second) = sessions(provider);
    let mut committed_rows = 1;
    for (case, change, explicit, expected, inherit) in [
        (0, "attributes", false, "00000", false),
        (1, "attributes", true, "00000", true),
        (2, "member_drop", false, "XX000", true),
        (3, "member_drop", true, "00000", true),
        (4, "member_recreate", false, "XX000", true),
        (5, "member_recreate", true, "00000", true),
        (6, "grantor_drop", false, "42704", true),
        (7, "grantor_recreate", false, "42704", true),
    ] {
        let prefix = format!("e{isolation_index}_{case}");
        let (target, member, grantor, other) = (
            format!("{prefix}_target"),
            format!("{prefix}_member"),
            format!("{prefix}_grantor"),
            format!("{prefix}_other"),
        );
        sql(&first, &format!("CREATE ROLE {target}; CREATE ROLE {member}; CREATE ROLE {grantor}; CREATE ROLE {other}; GRANT {target} TO {grantor} WITH ADMIN TRUE, INHERIT FALSE, SET FALSE"));
        let original = first.durable.roles.read()[&member].identity();
        let target_id = first.durable.roles.read()[&target].identity();
        sql(&first, &format!("BEGIN; GRANT {target} TO {other}"));
        sql(&second, &format!("BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2); SET LOCAL ROLE {grantor}"));
        let mutation = match change {
            "attributes" => format!("ALTER ROLE {member} NOINHERIT"),
            "member_drop" => format!("DROP ROLE {member}"),
            "member_recreate" => format!("DROP ROLE {member}; CREATE ROLE {member}"),
            "grantor_drop" => format!("DROP ROLE {grantor}"),
            "grantor_recreate" => format!("DROP ROLE {grantor}; CREATE ROLE {grantor}"),
            _ => unreachable!(),
        };
        let statement = format!(
            "GRANT {target} TO {member}{}",
            if explicit { " WITH INHERIT TRUE" } else { "" }
        );
        let result;
        (second, result) = after_wait(
            &first,
            second,
            &statement,
            role_lock(&first, &target),
            &format!("{mutation}; COMMIT"),
        );
        if expected == "00000" {
            result.unwrap();
            sql(&second, "COMMIT");
            committed_rows += 1;
        } else {
            assert_eq!(
                result.unwrap_err().sqlstate(),
                Some(expected),
                "{provider}/{isolation}/{change}"
            );
            sql(&second, "ROLLBACK");
        }
        assert_eq!(sql(&first, "SELECT v FROM t").rows.len(), committed_rows);
        let memberships = first.durable.role_memberships.read();
        let row = memberships
            .values()
            .find(|row| row.role.identity() == target_id && row.member.identity() == original);
        if expected == "00000" {
            assert_eq!(row.unwrap().inherit_option, inherit);
        } else {
            assert!(row.is_none());
        }
        drop(memberships);
        if change == "member_recreate" {
            assert_ne!(first.durable.roles.read()[&member].identity(), original);
            assert_eq!(
                sql(
                    &first,
                    &format!("SELECT pg_has_role('{member}', '{target}', 'MEMBER') AS member")
                )
                .rows[0]["member"],
                Value::Bool(false)
            );
        }
    }
    let expected = membership_rows(&first);
    drop(second);
    drop(first);
    let reopened = reopen(provider, &directory.path().join("table-locks.db"));
    assert_eq!(membership_rows(&reopened), expected);
    assert_eq!(sql(&reopened, "SELECT v FROM t").rows.len(), committed_rows);
}

#[test]
fn target_wait_preserves_previously_authorized_grantor_after_its_admin_grant_is_revoked() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE target; CREATE ROLE member; CREATE ROLE grantor; GRANT target TO grantor WITH ADMIN TRUE, INHERIT FALSE, SET FALSE");
            sql(&first, "BEGIN; REVOKE target FROM grantor");
            sql(&second, &format!("BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2); SET LOCAL ROLE grantor"));
            let (second, result) = after_wait(
                &first,
                second,
                "GRANT target TO member",
                role_lock(&first, "target"),
                "COMMIT",
            );
            result.unwrap();
            sql(&second, "COMMIT");
            let rows = membership_rows(&first);
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0]["grantor"],
                Value::Int(first.durable.roles.read()["grantor"].oid)
            );
            assert_eq!(
                rows[0]["member"],
                Value::Int(first.durable.roles.read()["member"].oid)
            );
            drop(second);
            drop(first);
            let reopened = reopen(provider, &directory.path().join("table-locks.db"));
            assert_eq!(membership_rows(&reopened), rows);
            assert_eq!(sql(&reopened, "SELECT v FROM t").rows.len(), 2);
        }
    }
}
