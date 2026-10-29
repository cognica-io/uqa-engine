//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::resolution::{RelationLookupMode, RelationNameResolution},
    plan::{CtePlan, UnifiedPlan},
};
use context::{PrivilegeCatalog, PrivilegeCteCatalog, PrivilegeRelation, PrivilegeRelationKind};
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryBudget, Produced},
    CancellationToken,
};

struct Catalog {
    columns: Vec<String>,
    subject: RoleReference,
    reads: Cell<usize>,
    fail_at: Cell<usize>,
    unwind: Cell<bool>,
}

impl Catalog {
    fn new(columns: Vec<String>) -> Self {
        Self {
            columns,
            subject: "reader".into(),
            reads: Cell::new(0),
            fail_at: Cell::new(usize::MAX),
            unwind: Cell::new(false),
        }
    }
    fn scope<'a>(&'a self, control: &ProductionControl<'_>) -> CteScope<'a, &'a [QueryPlan]> {
        let namespace = RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: self.subject.clone(),
            lookup_mode: RelationLookupMode::Dynamic,
        };
        CteScope::borrowed_with_control(
            self,
            namespace.copy_with_control(control).unwrap(),
            self,
            &[],
            control,
        )
    }
}

impl PrivilegeCatalog for Catalog {
    fn relation(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<PrivilegeRelation>, SQLError> {
        panic!("controlled lineage must not call the ordinary metadata producer")
    }
    fn relation_with_control(
        &self,
        _: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<PrivilegeRelation>>, SQLError> {
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() == self.fail_at.get() {
            assert!(!self.unwind.get(), "catalog consumer unwind");
            return Err(SQLError::Routine {
                sqlstate: "XX005".into(),
                message: "selected catalog failure".into(),
            });
        }
        PrivilegeRelation::from_names(
            name,
            self.columns.iter().map(String::as_str),
            PrivilegeRelationKind::Table,
            control,
        )
        .map(Some)
    }
    fn has_select_privilege(
        &self,
        _: &RelationNameResolution,
        _: &PrivilegeRelation,
        _: Option<&str>,
        _: &RoleReference,
    ) -> Result<bool, SQLError> {
        Ok(true)
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
        panic!("query");
    };
    *query
}

#[test]
fn lineage_owns_exact_names_nodes_and_buffers_until_drop() {
    let catalog = Catalog::new(vec!["id".into(), "secret".into()]);
    let query = query("SELECT * FROM public.left_source a NATURAL JOIN public.right_source b");
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block");
    };
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = budget.reserve(37).unwrap();
    let scope = catalog.scope(&control);
    let baseline = budget.used();
    let mut universe = SourceLineage::default();
    let mut required = Required::new();
    let mut lineage = None;
    let allocation = allocation_counter::measure(|| {
        lineage = Some(
            analyze_source_lineage(
                block.from.as_ref().unwrap(),
                &[],
                &[],
                &scope,
                &mut universe,
                &mut required,
            )
            .unwrap(),
        );
    });
    assert_eq!(allocation.bytes_current, (budget.used() - baseline) as i64);
    let lineage = lineage.unwrap();
    assert_eq!(
        lineage
            .output
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "secret"]
    );
    assert_eq!(required.len(), 4);
    assert_eq!(lineage.tables.len(), 2);
    drop((lineage, universe, required));
    assert_eq!(budget.used(), baseline);
    drop(scope);
    assert_eq!(budget.used(), 37);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn wide_catalog_names_and_output_labels_reject_before_destination_growth() {
    let budget = MemoryBudget::new(512);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(37).unwrap();
    let wide = "wide_column_".repeat(256 * 1024);
    let mut derived = query("SELECT * FROM (SELECT 1 AS label) AS derived");
    let RelationalPlan::QueryBlock(block) = &mut derived.root else {
        panic!("block");
    };
    let SourcePlan::Subquery { body, .. } = block.from.as_mut().unwrap() else {
        panic!("source");
    };
    let RelationalPlan::QueryBlock(inner) = &mut body.root else {
        panic!("inner");
    };
    inner.projections[0].alias = Some(wide.clone());
    for (catalog, query) in [
        (
            Catalog::new(vec!["id".into(), wide]),
            query("SELECT id FROM public.input"),
        ),
        (Catalog::new(vec![]), derived),
    ] {
        let scope = catalog.scope(&control);
        let baseline = budget.used();
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                ensure_select_privileges_for_query(&query, &scope)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
        assert_eq!(budget.used(), baseline);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                ensure_select_privileges_for_query(&query, &scope)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(budget.used(), baseline);
            token.reset();
        }
    }
    assert_eq!(budget.used(), 37);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn catalog_error_and_consumer_unwind_release_partial_join_lineage() {
    let catalog = Catalog::new(vec!["id".into(), "secret".into()]);
    let query = query("WITH c AS (SELECT id FROM public.unread) SELECT a.id FROM public.left_source a JOIN public.right_source b USING (id)");
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let scope = catalog.scope(&control);
    let baseline = budget.used();
    catalog.fail_at.set(2);
    assert_eq!(
        ensure_select_privileges_for_query(&query, &scope)
            .unwrap_err()
            .sqlstate(),
        Some("XX005")
    );
    assert_eq!(catalog.reads.get(), 2);
    assert_eq!(budget.used(), baseline);
    catalog.reads.set(0);
    catalog.unwind.set(true);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ensure_select_privileges_for_query(&query, &scope)
    }))
    .is_err());
    assert_eq!(budget.used(), baseline);
    catalog.unwind.set(false);
    catalog.fail_at.set(usize::MAX);
    ensure_select_privileges_for_query(&query, &scope).unwrap();
    assert_eq!(budget.used(), baseline);
    drop(scope);
    assert_eq!(budget.used(), 0);
}
