//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod fixtures;
use fixtures::Fixture;

#[test]
fn all_schema_routine_privileges_share_atomic_acl_publication() {
    let fixture = Fixture::new();
    fixture.add("app.z", 101, false);
    fixture.add("app.a", 102, false);
    fixture.add("app.p", 103, true);
    fixture
        .grant("REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA app FROM PUBLIC")
        .unwrap();
    assert!(!fixture.allowed("app.z"));
    assert!(!fixture.allowed("app.a"));
    assert!(fixture.allowed("app.p"));
    fixture
        .grant("REVOKE ALL ON ALL PROCEDURES IN SCHEMA app FROM PUBLIC")
        .unwrap();
    assert!(!fixture.allowed("app.p"));
    fixture
        .grant("GRANT EXECUTE ON ALL ROUTINES IN SCHEMA app TO reader WITH GRANT OPTION")
        .unwrap();
    for name in ["app.z", "app.a", "app.p"] {
        assert!(fixture.allowed(name));
    }
    fixture.fail_persist.set(true);
    assert!(fixture
        .grant("REVOKE ALL ON ALL ROUTINES IN SCHEMA app FROM reader")
        .is_err());
    for name in ["app.z", "app.a", "app.p"] {
        assert!(fixture.allowed(name));
    }
    assert_eq!(fixture.persisted.borrow().len(), 3);
}

#[test]
fn schema_and_role_diagnostics_precede_acl_mutation_in_postgresql_order() {
    let fixture = Fixture::new();
    fixture.add("app.f", 101, false);
    for (sql, state, message) in [
        (
            "GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA absent TO missing",
            "3F000",
            "schema \"absent\" does not exist",
        ),
        (
            "GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA absent TO missing GRANTED BY reader",
            "0A000",
            "grantor must be current user",
        ),
        (
            "GRANT SELECT ON ALL FUNCTIONS IN SCHEMA absent TO PUBLIC",
            "3F000",
            "schema \"absent\" does not exist",
        ),
        (
            "GRANT SELECT ON ALL FUNCTIONS IN SCHEMA empty TO PUBLIC",
            "0LP01",
            "invalid privilege type SELECT for function",
        ),
        (
            "GRANT EXECUTE ON ALL ROUTINES IN SCHEMA empty TO missing",
            "42704",
            "role \"missing\" does not exist",
        ),
        (
            "GRANT ALL ON ALL ROUTINES IN SCHEMA app,absent TO reader",
            "3F000",
            "schema \"absent\" does not exist",
        ),
    ] {
        let error = fixture.grant(sql).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}");
        assert_eq!(error.to_string(), message, "{sql}");
    }
    assert!(fixture.persisted.borrow().is_empty());
    fixture
        .grant("GRANT EXECUTE ON ALL ROUTINES IN SCHEMA empty TO PUBLIC WITH GRANT OPTION")
        .unwrap();
    assert_eq!(
        fixture
            .grant("GRANT EXECUTE ON ALL ROUTINES IN SCHEMA app TO PUBLIC WITH GRANT OPTION")
            .unwrap_err()
            .sqlstate(),
        Some("0LP01")
    );
    fixture.denied_schema.set(true);
    assert_eq!(
        fixture
            .grant("GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA app,absent TO missing")
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
}

#[test]
fn repeated_schemas_retain_warning_order_and_empty_acl_authority_errors() {
    let fixture = Fixture::new();
    fixture.add("app.z", 101, false);
    fixture.add("app.a", 102, false);
    *fixture.current.borrow_mut() = "reader".into();
    fixture
        .grant("GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA app,app TO PUBLIC")
        .unwrap();
    let messages = fixture
        .notices
        .borrow()
        .iter()
        .map(|notice| notice.message.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        [
            "no privileges were granted for \"z\"",
            "no privileges were granted for \"a\"",
            "no privileges were granted for \"z\"",
            "no privileges were granted for \"a\""
        ]
    );
    *fixture.current.borrow_mut() = "uqa".into();
    fixture
        .grant("REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA app FROM PUBLIC")
        .unwrap();
    *fixture.current.borrow_mut() = "reader".into();
    let error = fixture
        .grant("GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA app TO PUBLIC")
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(error.to_string(), "permission denied for function z");
}

#[test]
fn writer_wait_follows_original_routine_identity_without_rebinding_names_or_new_members() {
    for replacement in [false, true] {
        let fixture = Fixture::new();
        fixture.add("app.f", 101, false);
        let mut next = fixture.registry.borrow().clone();
        let old = next.remove("app.f").unwrap().remove(0);
        let mut def = old.def.clone();
        if replacement {
            def.object_id = Some([44; 16]);
            def.catalog_oid = Some(200);
        } else {
            def.name = "app.renamed".into();
        }
        next.insert(
            def.name.clone(),
            vec![std::sync::Arc::new(
                uqa_sql::routines::SQLUserFunction::new(def, old.body.clone()),
            )],
        );
        *fixture.on_writer.borrow_mut() = Some(next);
        let result = fixture.grant("REVOKE ALL ON ALL FUNCTIONS IN SCHEMA app FROM PUBLIC");
        if replacement {
            assert_eq!(
                result.unwrap_err().to_string(),
                "cache lookup failed for function 101"
            );
            assert!(fixture.persisted.borrow().is_empty());
            assert!(fixture.allowed("app.f"));
        } else {
            result.unwrap();
            assert!(!fixture.allowed("app.renamed"));
        }
    }
}
