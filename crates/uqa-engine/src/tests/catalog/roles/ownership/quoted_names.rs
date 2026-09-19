//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literal owner names are distinct from current- and session-user specifications.

use super::{reopen, sessions, sql, TARGETS};

#[test]
fn quoted_object_owners_survive_publication_undo_and_reopen() {
    for provider in 0..3 {
        for target in TARGETS {
            let (directory, first, second) = sessions(provider);
            sql(
                &first,
                r#"CREATE ROLE "PUBLIC"; CREATE ROLE "CURRENT_USER"; CREATE ROLE "SESSION_USER"; CREATE ROLE actor SUPERUSER"#,
            );
            sql(&first, target.setup);
            sql(&first, "SET ROLE actor");
            for name in ["PUBLIC", "CURRENT_USER", "SESSION_USER"] {
                sql(&first, &target.alter(&format!(r#""{name}""#)));
                target.assert_owner(&second, name);
                sql(&first, "BEGIN; SAVEPOINT before_owner");
                sql(&first, &target.alter("CURRENT_USER"));
                target.assert_owner(&first, "actor");
                target.assert_owner(&second, name);
                sql(&first, "ROLLBACK TO before_owner; COMMIT");
                target.assert_owner(&second, name);
            }
            sql(&first, &target.alter("SESSION_USER"));
            target.assert_owner(&second, "uqa");
            sql(&first, &target.alter(r#""SESSION_USER""#));
            drop(second);
            drop(first);
            let reopened = reopen(provider, &directory.path().join("table-locks.db"));
            target.assert_owner(&reopened, "SESSION_USER");
        }
    }
}
