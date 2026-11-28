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
    expected_subject: Option<RoleReference>,
}

impl Catalog {
    fn new(columns: Vec<String>) -> Self {
        Self {
            columns,
            subject: "reader".into(),
            reads: Cell::new(0),
            fail_at: Cell::new(usize::MAX),
            unwind: Cell::new(false),
            expected_subject: None,
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
        subject: &RoleReference,
    ) -> Result<bool, SQLError> {
        if let Some(expected) = &self.expected_subject {
            assert_eq!(subject, expected);
        }
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

#[test]
fn target_privilege_scope_borrows_arenas_and_restores_inherited_authority() {
    let mut catalog = Catalog::new(vec!["id".into(), "secret".into()]);
    catalog.expected_subject = Some("captured".into());
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let scope = catalog.scope(&control);
    let before = budget.used();
    let mut unused = query("SELECT 1");
    let RelationalPlan::QueryBlock(block) = &mut unused.root else {
        panic!("block")
    };
    block.projections[0].expr = ScalarExpr::Literal(uqa_core::Value::Str("x".repeat(4 << 20)));
    let arena = [query("SELECT secret FROM public.input"), unused];
    let expressions = [
        ScalarExpr::ScalarSubquery(0),
        ScalarExpr::QualifiedColumn {
            qualifier: "input".into(),
            column: "id".into(),
        },
    ];
    let references = expressions.iter().collect::<Vec<_>>();
    let aliases = crate::ast::ReturningAliases::default();
    let subject = RoleReference::from("captured");
    let observed = allocation_counter::measure(|| {
        ensure_target_select(
            TargetSelectPrivilegeRequest {
                table: "public.input",
                privilege_subject: Some(&subject),
                target_qualifier: "target",
                returning_aliases: &aliases,
                expressions: &references,
                subqueries: &arena,
                required_columns: &[],
            },
            &scope,
        )
        .unwrap();
    });
    assert!(observed.bytes_total < 64 << 10, "{observed:?}");
    assert_eq!(observed.bytes_current, 0);
    assert_eq!(budget.used(), before);
    assert!(scope.scalar_subqueries.is_empty());
    assert_eq!(
        scope.privilege_subject().unwrap(),
        &RoleReference::from("reader")
    );
    drop(scope);
    assert_eq!(budget.used(), 0);
}

#[test]
fn discarded_projection_subqueries_preserve_relation_and_column_authority() {
    let catalog = Catalog::new(vec!["id".into(), "measurement".into()]);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let scope = catalog.scope(&control);
    let mut source = query("SELECT (SELECT s.measurement FROM public.samples AS s WHERE s.id = o.id), EXISTS (SELECT 1 FROM public.events), o.id IN (SELECT id FROM public.lookup) FROM public.outer_input AS o");
    let check = |source: &QueryPlan| {
        let mut universe = SourceLineage::default();
        let mut required = Required::new();
        analyze_query_plan(source, &scope, &[], &mut universe, &mut required).unwrap();
        let tables = universe
            .tables
            .iter()
            .map(|(table, _)| table.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            tables,
            [
                "public.events",
                "public.lookup",
                "public.outer_input",
                "public.samples"
            ]
        );
        let columns = required
            .iter()
            .map(|col| (col.table.as_str(), col.column.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            columns,
            [
                ("public.lookup", "id"),
                ("public.outer_input", "id"),
                ("public.samples", "id"),
                ("public.samples", "measurement")
            ]
        );
        ensure_required_select(&universe, &required, &scope).unwrap();
    };
    check(&source);
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        panic!("query block")
    };
    block
        .preserve_projection_privileges([2, 0], &control)
        .unwrap();
    block
        .preserve_projection_privileges([1, 0], &control)
        .unwrap();
    assert_eq!(block.privilege_subqueries, [0, 1, 2]);
    for projection in &mut block.projections {
        projection.expr = ScalarExpr::Literal(uqa_core::Value::Null);
    }
    crate::plan::subqueries::prune_query_with_control(&mut source, control).unwrap();
    check(&source);
    drop(scope);
    assert_eq!(memory.used(), 0);
}

#[test]
fn discarded_projection_subqueries_keep_cte_authority_without_materializing_definitions() {
    let catalog = Catalog::new(vec!["id".into()]);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let scope = catalog.scope(&control);
    let mut plan = query("WITH cold AS (SELECT id FROM public.samples), chain AS (SELECT (SELECT id FROM cold) AS id) SELECT (SELECT id FROM chain) AS discarded, 7 AS used");

    let verify = |plan: &QueryPlan, executed: &[bool]| {
        let analysis = CteAnalysis::query(plan, control).unwrap();
        assert_eq!(analysis.reachable().unwrap().as_slice(), executed);
        for index in 0..plan.ctes.len() {
            assert!(analysis.single_reference(index));
        }
        let mut universe = SourceLineage::default();
        let mut required = Required::new();
        analyze_query_plan(plan, &scope, &[], &mut universe, &mut required).unwrap();
        assert_eq!(
            universe
                .tables
                .iter()
                .map(|(table, _)| table.as_str())
                .collect::<Vec<_>>(),
            ["public.samples"],
        );
        assert_eq!(
            required
                .iter()
                .map(|col| (col.table.as_str(), col.column.as_str()))
                .collect::<Vec<_>>(),
            [("public.samples", "id")],
        );
        ensure_required_select(&universe, &required, &scope).unwrap();
    };
    verify(&plan, &[true, true]);

    let RelationalPlan::QueryBlock(block) = &mut plan.root else {
        panic!("block")
    };
    block.preserve_projection_privileges([0], &control).unwrap();
    let shared = std::mem::replace(
        &mut block.projections[0].expr,
        ScalarExpr::Literal(uqa_core::Value::Null),
    );
    verify(&plan, &[false, false]);

    let RelationalPlan::QueryBlock(block) = &mut plan.root else {
        panic!("block")
    };
    block.projections[1].expr = shared;
    verify(&plan, &[true, true]);

    let crate::plan::CtePlanBody::Query(chain) = &mut plan.ctes[1].body else {
        panic!("CTE query")
    };
    let RelationalPlan::QueryBlock(block) = &mut chain.root else {
        panic!("CTE block")
    };
    block.preserve_projection_privileges([0], &control).unwrap();
    block.projections[0].expr = ScalarExpr::Literal(uqa_core::Value::Null);
    verify(&plan, &[false, true]);
    drop(scope);
    assert_eq!(memory.used(), 0);
}

#[test]
fn retained_subquery_authorizes_block_and_lateral_input_columns_in_their_own_scopes() {
    let catalog = Catalog::new(vec!["id".into()]);
    let control = ProductionControl::uncontrolled();
    let scope = catalog.scope(&control);
    let mut plan = query("SELECT (SELECT 1 FROM generate_series(1, (SELECT id)) AS g(n) CROSS JOIN public.inner_input) FROM public.outer_input");
    let RelationalPlan::QueryBlock(outer) = &mut plan.root else {
        panic!("outer query block")
    };
    let RelationalPlan::QueryBlock(inner) = &mut outer.subqueries[0].root else {
        panic!("inner query block")
    };
    assert_eq!(inner.subqueries.len(), 1);
    inner.projections[0].expr = ScalarExpr::ScalarSubquery(0);
    inner.preserve_projection_privileges([0], &control).unwrap();
    inner.projections[0].expr = ScalarExpr::Literal(uqa_core::Value::Null);

    let mut universe = SourceLineage::default();
    let mut required = Required::new();
    analyze_query_plan(&plan, &scope, &[], &mut universe, &mut required).unwrap();
    let columns: Vec<_> = required
        .iter()
        .map(|column| (column.table.as_str(), column.column.as_str()))
        .collect();
    assert_eq!(
        columns,
        [("public.inner_input", "id"), ("public.outer_input", "id")]
    );
}
