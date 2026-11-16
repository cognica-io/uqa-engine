//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FromClause, LockStrength, LockWait, LockingClause},
    plan::{
        copy::lowering::query::block, retirement::construction::Prefix, ComputePlan,
        NoRegisteredAggregates, QueryBlockPlan, QueryPlan, SourcePlan,
    },
};

pub(super) fn block_input<'s: 'a, 'c: 'a, 'a, I: block::Input<'s, 'c> + Send + 'a>(
    source: &'s SelectStmt,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Prefix<Box<QueryBlockPlan>>>>> {
    Ok(construction::Read::new(copying.control, async move {
        let mut scope = copying.enter_plan_node()?;
        let block =
            scope.boxed(|_| Ok(crate::plan::mutable::construction::empty_projection_block()))?;
        let mut block = scope.retain_plan(block);
        block::construct(source, &mut scope, input, &mut block).await?;
        Ok(block)
    })?)
}

impl<'s: 'n, 'n, 'c> block::Input<'s, 'c> for Native<'n> {
    async fn source(
        &mut self,
        source: &'s FromClause,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<SourcePlan>> {
        source_input(source, copying, self)?.await
    }

    async fn finish_arena(&mut self, copying: &mut Copying<'c>) -> Result<Prefix<Vec<QueryPlan>>> {
        self.work.checkpoint().await;
        // The callback fixture retains query identities separately; enclosing field tests verify their exact order.
        let queries = copying.field("subqueries", |_| Ok(Vec::new()))?;
        Ok(copying.retain_plan(queries))
    }

    async fn compute(
        &mut self,
        block: &QueryBlockPlan,
        control: &ProductionControl<'c>,
    ) -> Result<ComputePlan> {
        block
            .compute_with_work(control, &mut self.work, &mut |expression| {
                crate::semantics::aggregates::is_aggregate_with_control(
                    &NoRegisteredAggregates,
                    expression,
                    control,
                )
            })
            .await
    }
}

pub(super) fn source_input<
    's: 'a,
    'c: 'a,
    'a,
    I: super::super::super::source::leaf::Input<'s> + Send + 'a,
>(
    source: &'s FromClause,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Prefix<SourcePlan>>>> {
    Ok(construction::Read::new(copying.control, async move {
        let mut scope = copying.enter_plan_node()?;
        let mut field = scope.begin_input_field()?;
        let source =
            super::super::super::source::leaf::construct(source, &mut field, input).await?;
        scope.finish_input_field("source", field);
        Ok(scope.retain_plan(source.take()))
    })?)
}

pub(super) fn query(sql: &str) -> SelectStmt {
    let Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("query fixture")
    };
    *query
}

fn scalar_query(value: i64) -> Expr {
    Expr::ScalarSubquery(Box::new(query(&format!("SELECT {value}"))))
}

fn fixture() -> SelectStmt {
    let mut source = query("SELECT 1 WINDOW named AS (PARTITION BY (SELECT 2))");
    source.projections[0].expr = scalar_query(1);
    source.projections[0].alias = Some("alias_é🙂".repeat(1024));
    source.from = query("SELECT * FROM (VALUES ((SELECT 3))) AS source(n)").from;
    source.r#where = Some(scalar_query(4));
    source.group_by = vec![scalar_query(5)];
    source.grouping_sets = vec![vec![scalar_query(6)], vec![]];
    source.group_distinct = true;
    source.having = Some(scalar_query(7));
    source.order_by = query("SELECT 1 ORDER BY (SELECT 8) DESC NULLS FIRST").order_by;
    source.limit = Some(scalar_query(9));
    source.with_ties = true;
    source.offset = Some(scalar_query(10));
    source.distinct = true;
    source.distinct_on = vec![scalar_query(11)];
    source.locking = vec![
        LockingClause {
            strength: LockStrength::ForUpdate,
            wait: LockWait::SkipLocked,
            relations: vec!["relation_é🙂".repeat(1024), "source".into()],
        },
        LockingClause {
            strength: LockStrength::ForKeyShare,
            wait: LockWait::NoWait,
            relations: vec![],
        },
    ];
    source
}

#[test]
fn retained_query_blocks_preserve_order_flags_locking_and_query_local_fields() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for (source, expected_queries) in [
        (fixture(), (1..=11).collect::<Vec<_>>()),
        (query("SELECT 1"), vec![]),
        (query("SELECT sum(1)"), vec![]),
        (query("SELECT row_number() OVER ()"), vec![]),
    ] {
        let RelationalPlan::QueryBlock(mut expected) =
            crate::plan::query::lower_relational_root(source.clone(), &NoRegisteredAggregates)
        else {
            unreachable!()
        };
        assert_eq!(expected.subqueries.len(), expected_queries.len());
        expected.subqueries.clear();
        let ((output, memory, queries), polls) = complete(&mut input(
            Arc::new(Source::Block(Box::new(source))),
            &native,
            &work,
            &token,
            &token,
        ));
        let Field::Block(block) = &*output else {
            unreachable!()
        };
        assert_eq!(
            serde_json::to_value(&***block).unwrap(),
            serde_json::to_value(&*expected).unwrap()
        );
        assert_eq!(&*queries, &expected_queries);
        assert!(polls > expected_queries.len() + 1);
        assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
        drop((memory, queries));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_query_blocks_release_every_abandoned_cancelled_or_denied_prefix() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let source = Arc::new(Source::Block(Box::new(fixture())));
    let (output, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(
                    advance(&mut reader).err().unwrap().sqlstate(),
                    Some("57014")
                );
            }
            drop(reader);
            assert_eq!((native.used(), work.used()), (37, 0));
        }
    }
    drop(sibling);
    for bytes in [0, 1, 512, 4096, 8192] {
        let denied = MemoryBudget::new(bytes);
        let error = input(source.clone(), &denied, &work, &token, &token)
            .read(&mut std::io::empty())
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
}

#[test]
fn retained_query_blocks_bound_empty_wide_locking_and_aggregate_observation() {
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for count in [0, 2048] {
        let mut source = query("SELECT 1");
        source.projections.clear();
        source.windows = vec![query("SELECT 1 WINDOW named AS ()").windows.remove(0); count];
        source.locking = vec![
            LockingClause {
                strength: LockStrength::ForShare,
                wait: LockWait::Block,
                relations: vec![],
            };
            count
        ];
        let ((output, memory, queries), polls) = complete(&mut input(
            Arc::new(Source::Block(Box::new(source))),
            &native,
            &work,
            &token,
            &token,
        ));
        let Field::Block(block) = &*output else {
            unreachable!()
        };
        assert_eq!(block.windows.len(), count);
        assert_eq!(block.locking.len(), count);
        assert!(matches!(block.compute, ComputePlan::Project));
        assert!(polls > count * 2 + 1);
        drop((output, memory, queries));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_query_blocks_retire_deep_fields_after_late_scalar_or_locking_failure() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for locking_failure in [false, true] {
                let mut source = query("SELECT 1");
                let mut expression = Expr::Literal(Value::Str("leaf".into()));
                for _ in 0..8192 {
                    expression = Expr::Not(Box::new(expression));
                }
                source.projections[0].expr = expression;
                if locking_failure {
                    source.locking = vec![LockingClause {
                        strength: LockStrength::ForUpdate,
                        wait: LockWait::Block,
                        relations: vec!["reject_source_alias".into()],
                    }];
                } else {
                    source.distinct_on = vec![scalar_query(99)];
                }
                let native = MemoryBudget::new(64 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let error = input(
                    Arc::new(Source::Block(Box::new(source))),
                    &native,
                    &work,
                    &token,
                    &token,
                )
                .read(&mut std::io::empty())
                .err()
                .unwrap();
                assert_eq!(
                    error.to_string(),
                    if locking_failure {
                        "internal error: rejected source alias"
                    } else {
                        "internal error: rejected scalar query"
                    }
                );
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
