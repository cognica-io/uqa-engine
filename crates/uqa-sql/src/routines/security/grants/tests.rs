//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{RoleAttribute, Statement},
    catalog::{roles::RoleReference, security::BoundSchemaSecurity},
    routines::{security::select_routine_acl_grantor, RoutineBody},
};
use std::{cell::RefCell, sync::Arc};

struct Names {
    denied: Vec<String>,
    visited: RefCell<Vec<String>>,
}
impl RoutineNameCatalog for Names {
    fn schema_security(&self, schema: &str) -> Option<BoundSchemaSecurity> {
        ["app", "other", "empty"]
            .contains(&schema)
            .then(|| BoundSchemaSecurity::bootstrap(schema))
    }
    fn current_role(&self) -> RoleReference {
        "owner".into()
    }
    fn search_path(&self) -> Vec<String> {
        vec!["app".into()]
    }
    fn require_schema_usage(&self, schema: &str, _: &RoleReference) -> Result<(), SQLError> {
        self.visited.borrow_mut().push(schema.into());
        if self.denied.iter().any(|denied| denied == schema) {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("permission denied for schema {schema}"),
            })
        } else {
            Ok(())
        }
    }
    fn schema_has_usage(&self, schema: &str, _: &RoleReference) -> bool {
        !self.denied.iter().any(|denied| denied == schema)
    }
    fn routine_type_display(&self, name: &str) -> String {
        name.into()
    }
    fn routine_identity_display(&self, oid: u32) -> Result<String, SQLError> {
        Ok(oid.to_string())
    }
}
fn names() -> Names {
    Names {
        denied: Vec::new(),
        visited: RefCell::new(Vec::new()),
    }
}
fn routine(name: &str, oid: u32, procedure: bool) -> Arc<SQLUserFunction> {
    let sql = if procedure {
        format!("CREATE PROCEDURE {name}() LANGUAGE SQL AS 'SELECT 1'")
    } else {
        format!("CREATE FUNCTION {name}() RETURNS integer LANGUAGE SQL AS 'SELECT 1'")
    };
    let Statement::CreateFunction(mut def) = crate::compile(&sql).unwrap().remove(0) else {
        panic!("routine");
    };
    def.catalog_oid = Some(oid);
    def.object_id = Some([oid as u8; 16]);
    def.owner = Some(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP);
    Arc::new(SQLUserFunction::new(*def, RoutineBody::Source))
}
fn registry() -> RoutineRegistry {
    [
        ("app.z", 101, false),
        ("app.a", 102, false),
        ("app.p", 103, true),
        ("other.f", 104, false),
    ]
    .into_iter()
    .map(|(name, oid, procedure)| (name.into(), vec![routine(name, oid, procedure)]))
    .collect()
}
fn selected(
    names: &Names,
    schemas: &[&str],
    kind: AlterRoutineKind,
) -> Result<Vec<(String, usize)>, SQLError> {
    routines_in_schemas(
        names,
        &registry(),
        &schemas
            .iter()
            .map(|name| (*name).into())
            .collect::<Vec<_>>(),
        kind,
        |routine| Ok(i64::from(routine.def.catalog_oid.unwrap())),
    )
}

#[test]
fn all_function_kind_includes_aggregates_and_windows_but_not_procedures() {
    // Independently observed with CREATE AGGREGATE and pg_catalog.sum/row_number.
    for prokind in ['f', 'a', 'w', 'p'] {
        assert_eq!(
            kind_matches(AlterRoutineKind::Function, prokind),
            prokind != 'p'
        );
        assert_eq!(
            kind_matches(AlterRoutineKind::Procedure, prokind),
            prokind == 'p'
        );
        assert!(kind_matches(AlterRoutineKind::Routine, prokind));
    }
    for kind in [
        AlterRoutineKind::Function,
        AlterRoutineKind::Procedure,
        AlterRoutineKind::Routine,
    ] {
        assert!(!kind_matches(kind, 'x'));
    }
}

#[test]
fn schema_expansion_retains_written_order_repetitions_and_catalog_order() {
    let names = names();
    let functions = selected(&names, &["app", "other", "app"], AlterRoutineKind::Function).unwrap();
    assert_eq!(
        functions
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["app.z", "app.a", "other.f", "app.z", "app.a"]
    );
    assert_eq!(
        selected(&names, &["app"], AlterRoutineKind::Procedure).unwrap(),
        [("app.p".into(), 0)]
    );
    assert_eq!(
        selected(&names, &["app"], AlterRoutineKind::Routine)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        selected(&names, &["empty"], AlterRoutineKind::Routine)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn schema_usage_and_missing_schema_follow_written_order_before_recipient_checks() {
    let mut names = names();
    names.denied.push("app".into());
    let error = selected(&names, &["app", "missing"], AlterRoutineKind::Routine).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(error.to_string(), "permission denied for schema app");
    let error = selected(&names, &["missing", "app"], AlterRoutineKind::Routine).unwrap_err();
    assert_eq!(error.sqlstate(), Some("3F000"));
    assert_eq!(error.to_string(), "schema \"missing\" does not exist");
}

#[test]
fn no_execute_is_an_error_while_execute_without_grant_option_only_warns() {
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 20_000;
    reader.object_id = [2; 16];
    reader.attributes.remove(&RoleAttribute::Superuser);
    let roles = BTreeMap::from([
        ("uqa".into(), RoleDefinition::bootstrap()),
        ("reader".into(), reader),
    ]);
    let mut def = routine("app.f", 20_001, false).def.clone();
    assert_eq!(
        select_routine_acl_grantor(&def, "reader", &roles, &BTreeMap::new()).unwrap(),
        None
    );
    def.execute_acl = Some(Vec::new());
    let error = select_routine_acl_grantor(&def, "reader", &roles, &BTreeMap::new()).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(error.to_string(), "permission denied for function f");
    assert!(
        select_routine_acl_grantor(&def, "uqa", &roles, &BTreeMap::new())
            .unwrap()
            .is_some()
    );
}

#[test]
fn declared_privileges_keep_postgresql_diagnostics() {
    for (clause, message) in [
        ("SELECT", "invalid privilege type SELECT for routine"),
        (
            "EXECUTE(a)",
            "column privileges are only valid for relations",
        ),
        ("TEMPORARY", "invalid privilege type TEMP for routine"),
    ] {
        let Statement::GrantRoutine(stmt) = crate::compile(&format!(
            "GRANT {clause} ON ALL ROUTINES IN SCHEMA app TO PUBLIC"
        ))
        .unwrap()
        .remove(0) else {
            panic!("grant");
        };
        let error = validate_privileges(&stmt).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0LP01"));
        assert_eq!(error.to_string(), message);
    }
}
