//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema authority keeps private catalog records beside fresh committed namespaces.

use crate::tests::relation_lock_support::{sessions, sql};
use uqa_core::Value;

#[test]
fn fixed_snapshot_schema_api_refresh_keeps_private_authority() {
    for provider in 0..3 {
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE owner; CREATE SCHEMA kept; CREATE SCHEMA removed; CREATE SCHEMA transferred");
            sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; CREATE SCHEMA private_schema; DROP SCHEMA removed; GRANT USAGE ON SCHEMA kept TO reader; ALTER SCHEMA transferred OWNER TO owner"));
            let private = first.durable.schemas.snapshot();
            sql(&second, "CREATE SCHEMA published");
            // Host catalog reads refresh authority without starting another SQL command.
            assert!(
                first.has_schema("private_schema").unwrap(),
                "{provider}/{isolation}"
            );
            assert!(first.has_namespace("published").unwrap());
            assert!(!first.list_schemas().unwrap().contains(&"removed".into()));
            let refreshed = first.durable.schemas.snapshot();
            for name in ["kept", "transferred"] {
                assert_eq!(
                    refreshed[name], private[name],
                    "{provider}/{isolation}/{name}"
                );
            }
            sql(&second, "CREATE SCHEMA published_later");
            assert!(first.has_namespace("published_later").unwrap());
            assert!(first.has_schema("private_schema").unwrap());
            sql(&first, "COMMIT");
            let reopened = first.new_session().unwrap();
            assert!(reopened.has_schema("private_schema").unwrap());
            assert!(!reopened.has_schema("removed").unwrap());
            let rows = sql(&reopened, "SELECT has_schema_privilege('reader', 'kept', 'USAGE') AS acl, has_schema_privilege('owner', 'transferred', 'CREATE') AS owner");
            assert_eq!(rows.rows[0]["acl"], Value::Bool(true));
            assert_eq!(rows.rows[0]["owner"], Value::Bool(true));
        }
    }
}

#[test]
fn private_schema_changes_survive_peer_catalog_publication() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE owner; CREATE SCHEMA kept; CREATE SCHEMA removed; CREATE SCHEMA transferred; CREATE SCHEMA peer");
            sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT schemas; CREATE SCHEMA private_schema; DROP SCHEMA removed; GRANT USAGE ON SCHEMA kept TO reader; ALTER SCHEMA transferred OWNER TO owner"));
            sql(
                &second,
                "GRANT CREATE ON SCHEMA peer TO reader; CREATE SCHEMA published",
            );
            let result = sql(&first, "SELECT has_schema_privilege('reader', 'kept', 'USAGE') AS private_acl, has_schema_privilege('reader', 'peer', 'CREATE') AS peer_acl, has_schema_privilege('owner', 'transferred', 'CREATE') AS private_owner");
            for name in ["private_acl", "peer_acl", "private_owner"] {
                assert_eq!(
                    result.rows[0][name],
                    Value::Bool(true),
                    "{provider}/{isolation}/{name}"
                );
            }
            let schemas = first.durable.schemas.snapshot();
            assert!(
                schemas.contains_key("private_schema"),
                "{provider}/{isolation}"
            );
            assert!(schemas.contains_key("published"));
            assert!(!schemas.contains_key("removed"), "{provider}/{isolation}");
            drop(schemas);
            sql(&first, "ROLLBACK TO schemas");
            sql(&second, "CREATE SCHEMA after_undo");
            let result = sql(&first, "SELECT has_schema_privilege('reader', 'kept', 'USAGE') AS private_acl, has_schema_privilege('reader', 'peer', 'CREATE') AS peer_acl, has_schema_privilege('owner', 'transferred', 'CREATE') AS private_owner");
            assert_eq!(result.rows[0]["private_acl"], Value::Bool(false));
            assert_eq!(result.rows[0]["peer_acl"], Value::Bool(true));
            assert_eq!(result.rows[0]["private_owner"], Value::Bool(false));
            let schemas = first.durable.schemas.snapshot();
            assert!(!schemas.contains_key("private_schema"));
            assert!(schemas.contains_key("removed"));
            assert!(schemas.contains_key("after_undo"));
            sql(&first, "COMMIT");
        }
    }
}
