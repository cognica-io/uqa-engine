//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    resolution::candidates::SearchPathRead,
    security::{
        schema_inquiry::{GraphNamespaceRead, SchemaRegistryRead},
        BoundSchemaSecurity,
    },
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
};

mod type_names;

struct Catalog {
    path: RefCell<Vec<String>>,
    schemas: RefCell<BTreeMap<String, BoundSchemaSecurity>>,
    path_guard_expected: Cell<bool>,
    reads: RefCell<Vec<&'static str>>,
}

impl Catalog {
    fn new() -> Self {
        Self {
            path: RefCell::new(
                ["pg_catalog", "absent", "information_schema", "tenant"]
                    .map(str::to_string)
                    .to_vec(),
            ),
            schemas: RefCell::new(
                ["pg_catalog", "information_schema", "tenant"]
                    .map(|name| (name.into(), BoundSchemaSecurity::bootstrap(name)))
                    .into_iter()
                    .collect(),
            ),
            path_guard_expected: Cell::new(false),
            reads: RefCell::new(Vec::new()),
        }
    }
}
impl RelationCandidateState for Catalog {
    fn temporary_schema_name(&self) -> String {
        self.reads.borrow_mut().push("temporary");
        "pg_temp_42".into()
    }
    fn search_path(&self) -> SearchPathRead<'_> {
        self.reads.borrow_mut().push("path");
        Box::new(self.path.borrow())
    }
}
impl SchemaPrivilegeCatalog for Catalog {
    fn refresh_namespace_catalog(&self) -> Result<(), SQLError> {
        panic!("pure creation selection cannot refresh catalogs")
    }
    fn schemas(&self) -> SchemaRegistryRead<'_> {
        assert_eq!(
            self.path.try_borrow_mut().is_err(),
            self.path_guard_expected.get()
        );
        self.reads.borrow_mut().push("schemas");
        Box::new(self.schemas.borrow())
    }
    fn graphs(&self) -> Box<dyn GraphNamespaceRead + '_> {
        panic!("raw API creation uses loaded schema names")
    }
    fn temporary_namespace_oids(
        &self,
    ) -> Option<crate::catalog::temporary_namespace::TemporaryNamespaceOids> {
        panic!("creation selection does not inspect allocation")
    }
    fn temporary_schema_name(&self) -> String {
        RelationCandidateState::temporary_schema_name(self)
    }
}

#[test]
fn api_creation_retains_search_path_until_loaded_schema_selection() {
    let catalog = Catalog::new();
    catalog.path_guard_expected.set(true);
    assert_eq!(
        api_relation_name(&catalog, &catalog, "docs").unwrap(),
        "tenant.docs"
    );
    assert_eq!(&*catalog.reads.borrow(), &["path", "schemas"]);
    assert!(catalog.path.try_borrow_mut().is_ok());
    assert!(catalog.schemas.try_borrow_mut().is_ok());
    catalog.path_guard_expected.set(false);
    catalog.reads.borrow_mut().clear();
    assert_eq!(
        api_relation_name(&catalog, &catalog, r#"tenant."quoted.name""#).unwrap(),
        r#"tenant."quoted.name""#
    );
    assert_eq!(&*catalog.reads.borrow(), &["schemas"]);
    assert_eq!(
        api_relation_name(&catalog, &catalog, "absent.docs").unwrap_err(),
        "schema `absent` does not exist"
    );
}

#[test]
fn temporary_name_validation_reads_session_only_after_valid_syntax() {
    let catalog = Catalog::new();
    for name in ["", "a.b.c", "\"unterminated"] {
        assert!(matches!(
            temporary_creation_parts(&catalog, name),
            Err(SQLError::Unsupported(_))
        ));
    }
    assert!(catalog.reads.borrow().is_empty());
    for name in ["docs", "pg_temp.docs", "pg_temp_42.docs"] {
        assert_eq!(
            temporary_creation_parts(&catalog, name).unwrap(),
            ("pg_temp_42".into(), "docs".into())
        );
    }
    let error = temporary_creation_parts(&catalog, "public.docs").unwrap_err();
    assert!(
        matches!(error,SQLError::Unsupported(message) if message=="temporary relations cannot specify a schema name")
    );
    assert_eq!(
        &*catalog.reads.borrow(),
        &["temporary", "temporary", "temporary", "temporary"]
    );
}

#[test]
fn creation_diagnostics_distinguish_absent_explicit_and_effective_namespaces() {
    for (schema, message) in [
        (
            Some("missing".to_string()),
            "schema \"missing\" does not exist",
        ),
        (None, "no schema has been selected to create in"),
    ] {
        assert!(
            matches!(missing_creation_schema(schema),SQLError::Routine {sqlstate,message:actual} if sqlstate=="3F000" && actual==message)
        );
    }
    let catalog = Catalog::new();
    catalog.path.borrow_mut().clear();
    catalog.path_guard_expected.set(true);
    assert_eq!(
        api_relation_name(&catalog, &catalog, "docs").unwrap_err(),
        "no schema has been selected to create in"
    );
    assert!(catalog.path.try_borrow_mut().is_ok());
}

#[test]
fn relation_persistence_follows_the_temporary_namespaces_as_postgresql_adjusts_it() {
    use crate::ast::RelationPersistence::{Permanent, Temporary, Unlogged};
    let adjusted = |schema: &str, persistence| {
        adjusted_relation_persistence(schema, persistence, "pg_temp_7")
            .map_err(|error| (error.sqlstate().map(str::to_string), error.to_string()))
    };
    for schema in ["pg_temp_7", "pg_toast_temp_7"] {
        assert_eq!(adjusted(schema, Temporary), Ok(Temporary));
        assert_eq!(adjusted(schema, Permanent), Ok(Temporary));
        assert_eq!(
            adjusted(schema, Unlogged),
            Err((
                Some("42P16".into()),
                "only temporary relations may be created in temporary schemas".into()
            ))
        );
    }
    assert_eq!(
        adjusted("public", Temporary),
        Err((
            Some("42P16".into()),
            "cannot create temporary relation in non-temporary schema".into()
        ))
    );
    assert_eq!(adjusted("public", Permanent), Ok(Permanent));
    assert_eq!(adjusted("public", Unlogged), Ok(Unlogged));
    for persistence in [Temporary, Permanent] {
        assert_eq!(
            adjusted("pg_temp_8", persistence),
            Err((
                Some("42P16".into()),
                "cannot create relations in temporary schemas of other sessions".into()
            ))
        );
    }
}

#[test]
fn relations_are_refused_in_system_and_toast_namespaces() {
    for schema in ["pg_catalog", "pg_toast", "pg_toast_temp_7"] {
        let error =
            ensure_relation_namespace_writable(&RelationIdentity::new(schema, "t"), "pg_temp_7")
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
        assert_eq!(
            error.to_string(),
            format!("permission denied to create \"{schema}.t\"")
        );
    }
    for schema in [
        "public",
        "pg_temp_7",
        "information_schema",
        "pg_toast_temp_8",
    ] {
        ensure_relation_namespace_writable(&RelationIdentity::new(schema, "t"), "pg_temp_7")
            .unwrap();
    }
}
