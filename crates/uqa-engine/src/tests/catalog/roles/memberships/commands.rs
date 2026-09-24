//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn named_keywords_and_duplicate_grants_preserve_membership_identity_and_notice_order() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE target; CREATE ROLE \"CURRENT_USER\"; CREATE ROLE \"SESSION_USER\"; GRANT target TO \"CURRENT_USER\", CURRENT_USER");
        let rows = membership_rows(&first);
        assert_eq!(rows.len(), 2);
        let roles = first.durable.roles.read();
        for oid in [10, roles["CURRENT_USER"].oid] {
            assert!(rows.iter().any(|row| row["member"] == Value::Int(oid)));
        }
        drop(roles);
        sql(&first, "GRANT target TO \"CURRENT_USER\", CURRENT_USER");
        assert_eq!(first.take_sql_notices(), vec![
            ("NOTICE".into(), "role \"CURRENT_USER\" has already been granted membership in role \"target\" by role \"uqa\"".into()),
            ("NOTICE".into(), "role \"uqa\" has already been granted membership in role \"target\" by role \"uqa\"".into()),
        ]);
        assert_eq!(membership_rows(&first), rows);
        sql(
            &first,
            "REVOKE target FROM \"CURRENT_USER\", \"CURRENT_USER\"",
        );
        assert!(first.take_sql_notices().is_empty());
        sql(&first, "REVOKE target FROM \"SESSION_USER\"");
        assert_eq!(first.take_sql_notices(), vec![("WARNING".into(), "role \"SESSION_USER\" has not been granted membership in role \"target\" by role \"uqa\"".into())]);
        let remaining = membership_rows(&first);
        assert_eq!(remaining.len(), 1);
        drop(second);
        drop(first);
        assert_eq!(
            membership_rows(&reopen(provider, &directory.path().join("table-locks.db"))),
            remaining
        );
    }
}

#[test]
fn creation_memberships_and_alter_group_preserve_authority_and_statement_atomicity() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE target; CREATE ROLE creator CREATEROLE; CREATE ROLE member NOINHERIT; CREATE ROLE actor; GRANT target TO creator WITH ADMIN TRUE, INHERIT FALSE, SET FALSE; GRANT creator TO actor WITH INHERIT TRUE, SET FALSE; SET ROLE creator; CREATE ROLE created IN ROLE target ROLE member; RESET ROLE");
        let roles = first.durable.roles.read();
        let memberships = first.durable.role_memberships.read();
        let created = roles["created"].identity();
        let creator = memberships
            .values()
            .find(|row| row.role.identity() == created && row.member.name == "creator")
            .unwrap();
        assert_eq!(creator.grantor.oid, 10);
        assert!(creator.admin_option && !creator.inherit_option && !creator.set_option);
        let member = memberships
            .values()
            .find(|row| row.role.identity() == created && row.member.name == "member")
            .unwrap();
        assert!(!member.inherit_option);
        drop(memberships);
        drop(roles);
        sql(&first, "SET ROLE actor; ALTER GROUP target ADD USER member");
        let error = first
            .sql("ALTER GROUP target ADD USER missing_member", &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42704"));
        sql(&first, "RESET ROLE; SET ROLE member");
        let error = first
            .sql("ALTER GROUP target ADD USER missing_member", &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
        assert!(error
            .to_string()
            .contains("permission denied to alter role"));
        sql(&first, "RESET ROLE");
        let before = membership_rows(&first);
        let error = first
            .sql("CREATE ROLE failed IN ROLE target ROLE missing_member", &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42704"));
        assert!(!first.durable.roles.read().contains_key("failed"));
        assert_eq!(membership_rows(&first), before);
        drop(second);
        drop(first);
        let reopened = reopen(provider, &directory.path().join("table-locks.db"));
        assert!(!reopened.durable.roles.read().contains_key("failed"));
        assert_eq!(membership_rows(&reopened), before);
    }
}
