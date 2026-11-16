//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{CteBody, CteCycleClause, CTE},
    plan::{copy::lowering::ctes as constructor, CommandPlan, CteCyclePlan},
};

pub(super) fn list_input<'s: 'a, 'c: 'a, 'a, I: constructor::Input<'s, 'c> + Send + 'a>(
    source: &'s [CTE],
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Prefix<Vec<CtePlan>>>>> {
    Ok(construction::Read::new(
        copying.control,
        constructor::construct_list(source, copying, input),
    )?)
}

fn cte_input<'s: 'a, 'c: 'a, 'a, I: constructor::Input<'s, 'c> + Send + 'a>(
    source: &'s CTE,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Prefix<CtePlan>>>> {
    Ok(construction::Read::new(copying.control, async move {
        let mut scope = copying.enter_plan_node()?;
        constructor::construct(source, &mut scope, input).await
    })?)
}

fn cycle_input<'s: 'a, 'c: 'a, 'a, M, D>(
    source: &'s CteCycleClause,
    copying: &'a mut Copying<'c>,
    mark: &'a mut M,
    default: &'a mut D,
) -> Result<construction::Read<'a, Result<Pending<CteCyclePlan>>>>
where
    M: ScalarInput<&'s Expr> + TextInput<&'s str> + Send + 'a,
    D: ScalarInput<&'s Expr> + Send + 'a,
{
    Ok(construction::Read::new(
        copying.control,
        constructor::metadata::construct_cycle(source, copying, mark, default),
    )?)
}

impl<'s: 'a, 'c: 'a, 'a> constructor::Input<'s, 'c> for Native<'a, Arena<'a>> {
    async fn cte(&mut self, source: &'s CTE, copying: &mut Copying<'c>) -> Result<Prefix<CtePlan>> {
        cte_input(source, copying, self)?.await
    }

    async fn cycle(
        &mut self,
        source: &'s CteCycleClause,
        copying: &mut Copying<'c>,
    ) -> Result<Pending<CteCyclePlan>> {
        let mut mark = Native {
            queries: Some(Arena::new(copying, self.work.clone())),
            work: self.work.clone(),
        };
        let mut default = Native {
            queries: Some(Arena::new(copying, self.work.clone())),
            work: self.work.clone(),
        };
        let result = cycle_input(source, copying, &mut mark, &mut default)?.await;
        result
    }

    async fn query(
        &mut self,
        source: &'s SelectStmt,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<QueryPlan>> {
        child(construct::Source::Select(source), copying, &mut self.work).await
    }

    async fn command(
        &mut self,
        _: &'s CteBody,
        _: &mut Copying<'c>,
    ) -> Result<Prefix<CommandPlan>> {
        self.work.checkpoint().await;
        Err(SQLError::Internal("rejected mutation body callback".into()))
    }
}

fn recursive_query() -> SelectStmt {
    blocks::query("WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < 3) SEARCH DEPTH FIRST BY n SET seq CYCLE n SET mark TO 1 DEFAULT 0 USING path SELECT * FROM t")
}

#[test]
fn retained_query_ctes_preserve_cycle_scopes_and_each_suspended_prefix() {
    let mut source = recursive_query();
    let cycle = source.with[0].cycle.as_mut().unwrap();
    cycle.mark_value = Expr::ScalarSubquery(Box::new(blocks::query("SELECT NOT ((SELECT 1) = 2)")));
    cycle.mark_default = Expr::ScalarSubquery(Box::new(blocks::query("SELECT (SELECT 3)")));
    let source = Statement::Select(Box::new(source));
    assert_equivalent([source.clone()]);
    assert_prefix_retirement(source);
}

#[test]
fn retained_query_ctes_retire_deep_cycle_after_body_or_sibling_failure() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for failure in 0..3 {
                let mut source = recursive_query();
                let cycle = source.with[0].cycle.as_mut().unwrap();
                let mut mark = Expr::Literal(Value::Bool(true));
                for _ in 0..8192 {
                    mark = Expr::Not(Box::new(mark));
                }
                cycle.mark_value = mark;
                let expected = match failure {
                    0 => {
                        let mut body = blocks::query("SELECT 1");
                        body.projections[0].alias = Some("reject_source_alias".into());
                        source.with[0].body = CteBody::Query(Box::new(body));
                        "internal error: rejected source alias"
                    }
                    1 => {
                        source.with[0].body = blocks::query(
                            "WITH t AS (DELETE FROM dst RETURNING x) SELECT * FROM t",
                        )
                        .with
                        .remove(0)
                        .body;
                        "internal error: rejected mutation body callback"
                    }
                    _ => {
                        let mut sibling = blocks::query("WITH t AS (SELECT 1) SELECT * FROM t")
                            .with
                            .remove(0);
                        sibling.name = "reject_source_alias".into();
                        source.with.push(sibling);
                        "internal error: rejected source alias"
                    }
                };
                let native = MemoryBudget::new(32 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let sibling = native.reserve(37).unwrap();
                let error = input(
                    Arc::new(Statement::Select(Box::new(source))),
                    &native,
                    &work,
                    &token,
                    &token,
                    false,
                )
                .read(&mut std::io::empty())
                .err()
                .unwrap();
                assert_eq!(error.to_string(), expected);
                assert_eq!((native.used(), work.used()), (37, 0));
                drop(sibling);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
