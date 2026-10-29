//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::{
        resolution::{RelationLookupMode, RelationNameResolution},
        roles::RoleReference,
    },
    plan::{CtePlan, UnifiedPlan},
    semantics::privileges::context::{
        PrivilegeCatalog, PrivilegeCteCatalog, PrivilegeRelation, PrivilegeRelationKind,
    },
};
use std::cell::{Cell, RefCell};

struct Catalog {
    subject: RoleReference,
    reads: Cell<usize>,
    checks: Cell<usize>,
    foreign: bool,
    lookups: RefCell<Vec<(String, RelationLookupMode)>>,
    required_subjects: std::collections::BTreeMap<String, RoleReference>,
}

impl Catalog {
    fn new(foreign: bool) -> Self {
        Self {
            subject: "reader".into(),
            reads: Cell::new(0),
            checks: Cell::new(0),
            foreign,
            lookups: RefCell::new(Vec::new()),
            required_subjects: std::collections::BTreeMap::new(),
        }
    }
    fn scope(&self) -> PrivilegeScope<'_, &[QueryPlan]> {
        PrivilegeScope::borrowed(
            self,
            RelationNameResolution {
                search_path: vec!["public".into()],
                temporary_schema: "pg_temp_1".into(),
                temporary_namespace_allocated: false,
                current_user: self.subject.clone(),
                lookup_mode: RelationLookupMode::Dynamic,
            },
            self,
            &[],
        )
    }
}

impl PrivilegeCatalog for Catalog {
    fn relation(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<PrivilegeRelation>, SQLError> {
        self.reads.set(self.reads.get() + 1);
        self.lookups
            .borrow_mut()
            .push((name.into(), resolution.lookup_mode));
        let canonical = if resolution.lookup_mode == RelationLookupMode::Bound {
            crate::catalog::resolution::bound_relation_identity(name)?;
            name.to_string()
        } else if name.contains('.') {
            name.to_string()
        } else {
            format!("public.{name}")
        };
        Ok(Some(PrivilegeRelation {
            canonical,
            columns: vec!["id".into(), "secret".into()],
            kind: if self.foreign {
                PrivilegeRelationKind::ForeignTable
            } else {
                PrivilegeRelationKind::Table
            },
        }))
    }
    fn has_select_privilege(
        &self,
        _: &RelationNameResolution,
        relation: &PrivilegeRelation,
        column: Option<&str>,
        subject: &RoleReference,
    ) -> Result<bool, SQLError> {
        self.checks.set(self.checks.get() + 1);
        if self.foreign && column.is_some() {
            return Err(SQLError::Internal("missing foreign ACL metadata".into()));
        }
        Ok(column != Some("secret")
            && self
                .required_subjects
                .get(&relation.canonical)
                .is_none_or(|required| required == subject))
    }
}
impl PrivilegeCteCatalog for Catalog {
    fn is_visible_cte(&self, _: &str) -> bool {
        false
    }
    fn materialized_columns(&self, _: &str) -> Option<Vec<String>> {
        None
    }
    fn deferred_reference(&self, _: &str) -> Option<&CtePlan> {
        None
    }
    fn privilege_subject(&self) -> Result<&RoleReference, SQLError> {
        Ok(&self.subject)
    }
}
fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}
fn block(query: &QueryPlan) -> &QueryBlockPlan {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    block
}
fn child(source: &SourcePlan) -> &QueryBlockPlan {
    let SourcePlan::Subquery { body, .. } = source else {
        panic!("subquery")
    };
    block(body)
}

#[test]
fn complete_select_coverage_borrows_only_checked_original_nodes() {
    let catalog = Catalog::new(false);
    let query = query("SELECT id FROM (SELECT id FROM (SELECT id FROM input) AS first) AS second");
    let root = block(&query);
    let source = root.from.as_ref().unwrap();
    let coverage = check_query_source(root, source, &catalog.scope()).unwrap();
    assert_eq!(catalog.reads.get(), 2);
    assert_eq!(catalog.checks.get(), 1);
    let nested = child(source);
    let inner = coverage.query_source(nested).unwrap();
    assert!(inner.covers_query(nested, nested.from.as_ref().unwrap()));
    let copied = nested.clone();
    assert!(!inner.covers_query(&copied, nested.from.as_ref().unwrap()));
    assert!(coverage.query_source(&copied).is_none());
    let leaf = child(nested.from.as_ref().unwrap());
    assert!(inner
        .query_source(leaf)
        .unwrap()
        .covers_query(leaf, leaf.from.as_ref().unwrap()));
    assert_eq!(catalog.reads.get(), 2);
    assert_eq!(catalog.checks.get(), 1);
}

#[test]
fn complete_select_coverage_preserves_unused_and_filter_column_denials() {
    for sql in [
        "SELECT id FROM (SELECT id, secret FROM input) AS derived",
        "SELECT id FROM (SELECT id FROM input WHERE secret = 'one') AS derived",
    ] {
        let catalog = Catalog::new(false);
        let query = query(sql);
        let root = block(&query);
        let result = check_query_source(root, root.from.as_ref().unwrap(), &catalog.scope());
        assert!(
            matches!(result, Err(SQLError::Routine { sqlstate, message }) if sqlstate == "42501" && message == "permission denied for table input")
        );
    }
}

#[test]
fn zero_column_foreign_checks_keep_metadata_errors_after_table_grants() {
    let catalog = Catalog::new(true);
    let query = query("SELECT count(*) FROM (SELECT FROM foreign_input) AS derived");
    let root = block(&query);
    let result = check_query_source(root, root.from.as_ref().unwrap(), &catalog.scope());
    assert!(
        matches!(result, Err(SQLError::Internal(message)) if message == "missing foreign ACL metadata")
    );
    assert_eq!(catalog.checks.get(), 2);
}

#[test]
fn join_coverage_projects_only_original_children_and_declines_local_ctes() {
    let catalog = Catalog::new(false);
    let query = query(
        "SELECT l.id FROM (SELECT id FROM input) AS l CROSS JOIN (SELECT id FROM input) AS r",
    );
    let root = block(&query);
    let source = root.from.as_ref().unwrap();
    let coverage = check_query_source(root, source, &catalog.scope()).unwrap();
    let SourcePlan::Join { left, right, .. } = source else {
        panic!("join")
    };
    let left_coverage = coverage.join_child(false).unwrap();
    assert!(left_coverage.query_source(child(left)).is_some());
    assert!(left_coverage.query_source(child(right)).is_none());
    assert!(coverage
        .join_child(true)
        .unwrap()
        .query_source(child(right))
        .is_some());
    let with_cte = self::query(
        "SELECT id FROM (WITH c AS (SELECT id FROM input) SELECT id FROM c) AS derived",
    );
    let root = block(&with_cte);
    let source = root.from.as_ref().unwrap();
    let coverage = check_query_source(root, source, &catalog.scope()).unwrap();
    assert!(coverage.query_source(child(source)).is_none());
}

#[test]
fn nested_authorization_preserves_query_relation_identity_modes() {
    for enclosing in [RelationLookupMode::Bound, RelationLookupMode::Dynamic] {
        for column in ["id", "secret"] {
            let catalog = Catalog::new(false);
            let mut query = query(&format!("SELECT l.id FROM (SELECT id FROM public.input) AS l CROSS JOIN (SELECT {column} FROM input) AS r"));
            let RelationalPlan::QueryBlock(root) = &mut query.root else {
                panic!("block")
            };
            let SourcePlan::Join { left, .. } = root.from.as_mut().unwrap() else {
                panic!("join")
            };
            let SourcePlan::Subquery { body, .. } = left.as_mut() else {
                panic!("query")
            };
            body.relations_bound = true;
            let mut scope = catalog.scope();
            scope.resolution.set_lookup_mode(enclosing);
            let result = check_query_source(root, root.from.as_ref().unwrap(), &scope);
            assert_eq!(
                result
                    .err()
                    .and_then(|error| error.sqlstate().map(str::to_owned)),
                (column == "secret").then(|| "42501".to_string())
            );
            assert_eq!(
                &catalog.lookups.borrow()[..2],
                &[
                    ("public.input".into(), RelationLookupMode::Bound),
                    ("input".into(), RelationLookupMode::Dynamic)
                ]
            );
            assert_eq!(scope.resolution.lookup_mode, enclosing);
        }
    }
}

#[test]
fn nested_query_authority_is_checked_independently_of_its_action_owner() {
    for subject in ["caller", "reader"] {
        let mut catalog = Catalog::new(false);
        catalog
            .required_subjects
            .insert("public.input".into(), "caller".into());
        catalog
            .required_subjects
            .insert("public.action".into(), "reader".into());
        let mut query = query(
            "SELECT l.id FROM (SELECT id FROM input) AS l CROSS JOIN (SELECT id FROM action) AS r",
        );
        let RelationalPlan::QueryBlock(root) = &mut query.root else {
            panic!("block")
        };
        let SourcePlan::Join { left, .. } = root.from.as_mut().unwrap() else {
            panic!("join")
        };
        let SourcePlan::Subquery { body, .. } = left.as_mut() else {
            panic!("query")
        };
        body.relation_privilege_subject = Some(subject.into());
        let scope = catalog.scope();
        let result = check_query_source(root, root.from.as_ref().unwrap(), &scope);
        assert_eq!(
            result
                .err()
                .and_then(|error| error.sqlstate().map(str::to_owned)),
            (subject == "reader").then(|| "42501".into())
        );
        assert_eq!(
            scope.privilege_subject().unwrap(),
            &RoleReference::from("reader")
        );
        assert_eq!(scope.resolution.current_user, RoleReference::from("reader"));
    }
}

#[test]
fn complete_query_permissions_keep_declared_inputs_and_the_selected_owner() {
    for (sql, permitted) in [
        ("SELECT id FROM public.input", true),
        (
            "SELECT id FROM (SELECT id,secret FROM public.input) AS derived",
            false,
        ),
        (
            "WITH c AS (SELECT secret FROM public.input) SELECT * FROM c",
            false,
        ),
        ("SELECT id,1/0 FROM public.input", true),
    ] {
        for subject in ["owner", "reader"] {
            let mut catalog = Catalog::new(false);
            catalog
                .required_subjects
                .insert("public.input".into(), "owner".into());
            let mut query = query(sql);
            query.relation_privilege_subject = Some(subject.into());
            let scope = catalog.scope();
            let result =
                crate::semantics::privileges::ensure_select_privileges_for_query(&query, &scope);
            assert_eq!(
                result.is_ok(),
                permitted && subject == "owner",
                "{sql}: {subject}: {result:?}"
            );
            if let Err(error) = result {
                assert_eq!(error.sqlstate(), Some("42501"));
            }
            assert_eq!(
                scope.privilege_subject().unwrap(),
                &RoleReference::from("reader")
            );
        }
    }
}
