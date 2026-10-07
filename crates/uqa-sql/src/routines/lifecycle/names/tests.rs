//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Names(Vec<String>);
impl RoutineNameCatalog for Names {
    fn schema_security(&self, schema: &str) -> Option<BoundSchemaSecurity> {
        Some(BoundSchemaSecurity::bootstrap(schema))
    }
    fn current_role(&self) -> RoleReference {
        "uqa".into()
    }
    fn search_path(&self) -> Vec<String> {
        self.0.clone()
    }
    fn require_schema_usage(&self, _: &str, _: &RoleReference) -> Result<(), SQLError> {
        Ok(())
    }
    fn schema_has_usage(&self, _: &str, _: &RoleReference) -> bool {
        true
    }
    fn routine_type_display(&self, name: &str) -> String {
        name.into()
    }
    fn routine_identity_display(&self, oid: u32) -> Result<String, SQLError> {
        Ok(oid.to_string())
    }
}

#[test]
fn exact_catalog_lookup_excludes_coercions_defaults_and_procedures() {
    let mut identities = vec![];
    for (oid, schema, types, kind) in [
        (1, "public", vec![1009, 20], 'f'),
        (2, "public", vec![1009, 26, 23], 'f'),
        (3, "public", vec![1009, 26], 'p'),
        (4, "other", vec![1009, 26], 'f'),
    ] {
        identities.push(RoutineCatalogIdentity {
            oid,
            relation: RelationIdentity::new(schema, "validate"),
            argument_types: types,
            kind,
        });
    }
    let catalog = Names(vec!["public".into(), "other".into()]);
    assert_eq!(
        exact_function(
            &catalog,
            &identities,
            "validate",
            &[1009, 26],
            &["text[]".into(), "oid".into()]
        )
        .unwrap(),
        3
    );
    let error = exact_function(
        &catalog,
        &identities,
        "public.validate",
        &[1009, 26],
        &["text[]".into(), "oid".into()],
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "function public.validate(text[], oid) does not exist"
    );
}

#[test]
fn builtin_catalog_position_is_implicit_first_or_explicit() {
    let identities = ["public", "pg_catalog"]
        .into_iter()
        .enumerate()
        .map(|(position, schema)| RoutineCatalogIdentity {
            oid: position as u32 + 1,
            relation: RelationIdentity::new(schema, "version"),
            argument_types: vec![],
            kind: 'f',
        })
        .collect::<Vec<_>>();
    assert_eq!(
        exact_function(
            &Names(vec!["public".into()]),
            &identities,
            "version",
            &[],
            &[]
        )
        .unwrap(),
        1
    );
    assert_eq!(
        exact_function(
            &Names(vec!["public".into(), "pg_catalog".into()]),
            &identities,
            "version",
            &[],
            &[]
        )
        .unwrap(),
        0
    );
}
