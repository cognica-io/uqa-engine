//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use context::{PrivilegeCatalog, PrivilegeCteCatalog, PrivilegeRelation, PrivilegeRelationKind};
use std::cell::RefCell;
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::plan::{QueryPlan, RelationalPlan};
use uqa_sql::semantics::privileges::{context, ensure_select_privileges_for_query_block};
use uqa_sql::SQLError;

struct Catalog {
    width: usize,
    role: RoleReference,
    checked: RefCell<Vec<(String, Option<String>)>>,
}

impl Catalog {
    fn new(width: usize) -> Self {
        Self {
            width,
            role: "reader".into(),
            checked: RefCell::new(Vec::new()),
        }
    }
}

impl PrivilegeCatalog for Catalog {
    fn relation(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        name: &str,
    ) -> Result<Option<PrivilegeRelation>, SQLError> {
        Ok(Some(PrivilegeRelation {
            canonical: name.into(),
            columns: (0..self.width).map(|i| format!("c{i}")).collect(),
            kind: PrivilegeRelationKind::Table,
        }))
    }

    fn has_select_privilege(
        &self,
        _: &uqa_sql::catalog::resolution::RelationNameResolution,
        relation: &PrivilegeRelation,
        column: Option<&str>,
        subject: &RoleReference,
    ) -> Result<bool, SQLError> {
        assert_eq!(subject, &self.role);
        self.checked
            .borrow_mut()
            .push((relation.canonical.clone(), column.map(str::to_owned)));
        Ok(column != Some("c2"))
    }
}

impl PrivilegeCteCatalog for Catalog {
    fn is_visible_cte(&self, _: &str) -> bool {
        false
    }
    fn materialized_columns(&self, _: &str) -> Option<Vec<String>> {
        None
    }
    fn deferred_reference(&self, _: &str) -> Option<&uqa_sql::plan::CtePlan> {
        None
    }
    fn privilege_subject(&self) -> Result<&RoleReference, SQLError> {
        Ok(&self.role)
    }
}

fn scope(catalog: &Catalog) -> context::PrivilegeScope<'_> {
    context::PrivilegeScope::new(
        catalog,
        uqa_sql::catalog::resolution::RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: catalog.role.clone(),
            lookup_mode: uqa_sql::catalog::resolution::RelationLookupMode::Dynamic,
        },
        catalog,
        Vec::new(),
    )
}

fn plan(sql: &str) -> QueryPlan {
    let uqa_sql::plan::UnifiedPlan::Query(query) =
        uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("expected query")
    };
    *query
}

fn authorize(query: &QueryPlan, catalog: &Catalog) -> Result<(), SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("expected block")
    };
    ensure_select_privileges_for_query_block(block, block.from.as_ref().unwrap(), &scope(catalog))
}

#[test]
fn nested_privilege_analysis_does_not_copy_outer_column_lineage() {
    let catalog = Catalog::new(1024);
    let mut nested = plan("SELECT parent.c0");
    for _ in 1..24 {
        let mut next = plan("SELECT (SELECT 1)");
        let RelationalPlan::QueryBlock(block) = &mut next.root else {
            unreachable!()
        };
        block.subqueries[0] = nested;
        nested = next;
    }
    let mut query = plan("SELECT (SELECT 1) FROM root AS parent");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    block.subqueries[0] = nested;
    let allocation = allocation_counter::measure(|| authorize(&query, &catalog).unwrap());
    assert_eq!(
        *catalog.checked.borrow(),
        vec![("root".into(), Some("c0".into()))]
    );
    // The wide outer relation is retained once, independent of the nested scalar-block depth.
    assert!(allocation.bytes_total < 4 * 1024 * 1024, "{allocation:?}");
}

#[test]
fn borrowed_privilege_scopes_preserve_correlated_and_join_requirements() {
    for (sql, expected) in [
        ("SELECT (SELECT child.c1 FROM leaf AS child WHERE child.c0 = parent.c0) FROM root AS parent", vec![("leaf", "c0"), ("leaf", "c1"), ("root", "c0")]),
        ("SELECT parent.c0 FROM root AS parent JOIN leaf AS child ON parent.c1 = child.c1", vec![("leaf", "c1"), ("root", "c0"), ("root", "c1")]),
        ("SELECT c0 FROM root JOIN leaf USING (c0)", vec![("leaf", "c0"), ("root", "c0")]),
        ("SELECT q.c1 FROM root AS parent, LATERAL (SELECT child.c1 FROM leaf AS child WHERE child.c0 = parent.c0) AS q", vec![("leaf", "c0"), ("leaf", "c1"), ("root", "c0")]),
    ] {
        let catalog = Catalog::new(3);
        authorize(&plan(sql), &catalog).unwrap();
        let expected = expected.into_iter().map(|(table,column)| (table.to_string(),Some(column.to_string()))).collect::<Vec<_>>();
        assert_eq!(*catalog.checked.borrow(), expected, "{sql}");
    }
    let catalog = Catalog::new(3);
    let error = authorize(&plan("SELECT (SELECT child.c2 FROM leaf AS child WHERE child.c0 = parent.c0) FROM root AS parent"), &catalog).unwrap_err();
    assert!(matches!(error, SQLError::Routine { ref sqlstate, .. } if sqlstate == "42501"));
    assert_eq!(
        *catalog.checked.borrow(),
        vec![
            ("leaf".into(), Some("c0".into())),
            ("leaf".into(), Some("c2".into()))
        ]
    );
}

#[test]
fn nested_privilege_analysis_does_not_copy_inherited_subquery_plans() {
    let catalog = Catalog::new(3);
    let mut scope = scope(&catalog);
    let large = plan(&format!("SELECT '{}'", "x".repeat(32 * 1024)));
    scope.scalar_subqueries = vec![large; 32];
    let query = plan("SELECT (SELECT (SELECT parent.c0)) FROM root AS parent");
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    let allocation = allocation_counter::measure(|| {
        ensure_select_privileges_for_query_block(block, block.from.as_ref().unwrap(), &scope)
            .unwrap();
    });
    assert_eq!(
        *catalog.checked.borrow(),
        vec![("root".into(), Some("c0".into()))]
    );
    assert!(allocation.bytes_total < 256 * 1024, "{allocation:?}");
}

#[test]
fn nested_privilege_analysis_does_not_copy_local_cte_plans() {
    let catalog = Catalog::new(3);
    let query = plan(&format!(
        "SELECT (WITH held AS (SELECT '{}') SELECT (SELECT parent.c0)) FROM root AS parent",
        "x".repeat(1024 * 1024),
    ));
    let allocation = allocation_counter::measure(|| authorize(&query, &catalog).unwrap());
    assert_eq!(
        *catalog.checked.borrow(),
        vec![("root".into(), Some("c0".into()))]
    );
    assert!(allocation.bytes_total < 256 * 1024, "{allocation:?}");
}

#[test]
fn borrowed_privilege_query_arena_preserves_denials_and_check_order() {
    let query = plan("SELECT (SELECT child.c2 FROM leaf AS child WHERE child.c0 = parent.c0) FROM root AS parent");
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    let catalog = Catalog::new(3);
    let owned = scope(&catalog);
    let borrowed = context::PrivilegeScope::borrowed(
        owned.catalog,
        (*owned.resolution).clone(),
        owned.inherited,
        &block.subqueries,
    );
    assert!(std::ptr::eq(
        borrowed.scalar_subqueries,
        block.subqueries.as_slice()
    ));
    let error = uqa_sql::semantics::privileges::ensure_select_privileges_for_source_expressions(
        block.from.as_ref().unwrap(),
        &[&block.projections[0].expr],
        &borrowed,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(
        *catalog.checked.borrow(),
        vec![
            ("leaf".into(), Some("c0".into())),
            ("leaf".into(), Some("c2".into()))
        ]
    );
}
