//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FromClause, plan::copy::lowering::source};

pub(super) async fn source_child<'s: 'a, 'c: 'a, 'a>(
    source: &'s FromClause,
    copying: &mut Copying<'c>,
    input: &mut Native<'a, Arena<'a>>,
) -> Result<Prefix<SourcePlan>> {
    let mut field = copying.begin_input_field()?;
    let queries = input.queries.take();
    let output = input.work.submit(move |work| async move {
        let mut input = Native { work, queries };
        let source = source_input(source, &mut field, &mut input)?.await?;
        Ok::<_, SQLError>((source, field, input.queries))
    })?;
    let (source, field, queries) = construction::receive(output).await?;
    input.queries = queries;
    copying.finish_input_field("source", field);
    Ok(source)
}

fn source_input<'s: 'a, 'c: 'a, 'a, I: source::Input<'s, 'c> + Send + 'a>(
    source: &'s FromClause,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Prefix<SourcePlan>>>> {
    Ok(construction::Read::new(copying.control, async move {
        let mut scope = copying.enter_plan_node()?;
        source::construct(source, &mut scope, input).await
    })?)
}

impl<'s: 'a, 'c: 'a, 'a> source::Input<'s, 'c> for Native<'a, Arena<'a>> {
    async fn source(
        &mut self,
        source: &'s FromClause,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<SourcePlan>> {
        source_child(source, copying, self).await
    }
    async fn query(
        &mut self,
        source: &'s SelectStmt,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<QueryPlan>> {
        child(construct::Source::Select(source), copying, &mut self.work).await
    }
}

#[test]
fn retained_query_trees_preserve_join_cte_and_arena_ownership() {
    assert_equivalent([
        "SELECT a.n, b.n, (SELECT 1) FROM (SELECT (SELECT 2) AS n) a LEFT JOIN LATERAL (SELECT a.n + (SELECT 3) AS n) b ON b.n = (SELECT 4)",
        "SELECT * FROM (l JOIN r USING (n) AS merged) AS j(n, x, y)",
        "SELECT * FROM l NATURAL FULL JOIN r, LATERAL (SELECT (SELECT 5)) v",
        "SELECT * FROM l RIGHT JOIN r ON (SELECT true) CROSS JOIN (SELECT 7)",
        "WITH a(n) AS MATERIALIZED (SELECT (SELECT 1)), b AS NOT MATERIALIZED (SELECT NOT ((SELECT 2) = 3)) SELECT * FROM a JOIN b ON (SELECT true)",
        "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < 3) SEARCH DEPTH FIRST BY n SET seq CYCLE n SET mark TO 1 DEFAULT 0 USING path SELECT * FROM t",
        "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < 3) SEARCH BREADTH FIRST BY n SET seq CYCLE n SET mark USING path SELECT * FROM t",
        "WITH a AS (WITH b AS (SELECT (SELECT 1)) SELECT * FROM b) SELECT * FROM a",
    ].into_iter().map(statement));
}

#[test]
fn retained_query_trees_retire_each_join_cte_and_derived_prefix() {
    assert_prefix_retirement(statement("WITH a(n) AS (SELECT (SELECT 1)), b(n) AS (SELECT (SELECT 2)) SELECT (SELECT 3) FROM a LEFT JOIN LATERAL (SELECT (SELECT 4) FROM b) v(n) ON (SELECT true)"));
}

#[test]
fn retained_query_trees_bound_deep_children_and_late_metadata_failure() {
    // The caller retains source syntax; this thread qualifies constructor polling and plan retirement.
    for kind in 0..3 {
        for fail in [false, true] {
            let mut source = blocks::query("SELECT (SELECT 1) FROM leaf");
            for _ in 0..512 {
                match kind {
                    0 => {
                        let mut join = blocks::query("SELECT 1 FROM l JOIN r ON (SELECT true)")
                            .from
                            .unwrap();
                        let FromClause::Join { left, .. } = &mut join else {
                            unreachable!()
                        };
                        **left = source.from.take().unwrap();
                        source.from = Some(join);
                    }
                    1 => {
                        let mut parent = blocks::query("SELECT 1 FROM (SELECT 2) AS nested");
                        let FromClause::Subquery { body, .. } = parent.from.as_mut().unwrap()
                        else {
                            unreachable!()
                        };
                        **body = source;
                        source = parent;
                    }
                    _ => {
                        let mut parent =
                            blocks::query("WITH nested AS (SELECT 1) SELECT * FROM nested");
                        parent.with[0].body = crate::ast::CteBody::Query(Box::new(source));
                        source = parent;
                    }
                }
            }
            if fail {
                source.projections[0].alias = Some("reject_source_alias".into());
            }
            let source = Arc::new(Statement::Select(Box::new(source)));
            let retained_source = source.clone();
            std::thread::Builder::new()
                .stack_size(256 * 1024)
                .spawn(move || {
                    let native = MemoryBudget::new(64 << 20);
                    let work = MemoryBudget::new(1 << 20);
                    let token = CancellationToken::new();
                    let result = input(source, &native, &work, &token, &token, false)
                        .read(&mut std::io::empty());
                    if fail {
                        assert_eq!(
                            result.err().unwrap().to_string(),
                            "internal error: rejected source alias"
                        );
                    } else {
                        let (query, memory) = result.unwrap();
                        assert_eq!(allocation_counter::measure(|| drop(query)).count_total, 0);
                        drop(memory);
                    }
                    assert_eq!((native.used(), work.used()), (0, 0));
                })
                .unwrap()
                .join()
                .unwrap();
            drop(retained_source);
        }
    }
}
