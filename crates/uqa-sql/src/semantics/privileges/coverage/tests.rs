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
use std::cell::Cell;

struct Catalog {
    subject: RoleReference,
    reads: Cell<usize>,
    checks: Cell<usize>,
    foreign: bool,
}

impl Catalog {
    fn new(foreign: bool) -> Self {
        Self {
            subject: "reader".into(),
            reads: Cell::new(0),
            checks: Cell::new(0),
            foreign,
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
        _: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<PrivilegeRelation>, SQLError> {
        self.reads.set(self.reads.get() + 1);
        Ok(Some(PrivilegeRelation {
            canonical: name.into(),
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
        _: &PrivilegeRelation,
        column: Option<&str>,
        _: &RoleReference,
    ) -> Result<bool, SQLError> {
        self.checks.set(self.checks.get() + 1);
        if self.foreign && column.is_some() {
            return Err(SQLError::Internal("missing foreign ACL metadata".into()));
        }
        Ok(column != Some("secret"))
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
