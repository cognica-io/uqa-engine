//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{
    copy::lowering::{
        query::{block, construct},
        QueryArena,
    },
    retirement::construction::{Context, Prefix},
    ComputePlan, CtePlan, NoRegisteredAggregates, QueryBlockPlan, QueryPlan, SourcePlan,
};

mod ctes;
mod trees;

struct Arena<'a> {
    value: QueryArena<'a>,
    work: Work<'a>,
}

impl<'a> Arena<'a> {
    fn new<'c: 'a>(copying: &Copying<'c>, work: Work<'a>) -> Self {
        Self {
            value: copying.query_arena(),
            work,
        }
    }
}

impl<'s: 'a, 'a> crate::plan::scalar::ScalarQueryInput<&'s Expr, &'s SelectStmt> for Arena<'a> {
    async fn parameter(
        &mut self,
        expression: &'s Expr,
        control: &ProductionControl<'_>,
    ) -> Result<Option<usize>> {
        control.check()?;
        Ok(self
            .value
            .copying
            .parameters
            .and_then(|parameters| parameters(expression)))
    }

    async fn append(
        &mut self,
        query: &'s SelectStmt,
        control: &ProductionControl<'_>,
    ) -> Result<usize> {
        self.work.checkpoint().await;
        control.check()?;
        self.value.queries.reserve(1)?;
        let index = self.value.queries.len();
        let mut item = self.value.copying.begin_input_field()?;
        let query = child(construct::Source::Select(query), &mut item, &mut self.work).await?;
        self.value.copying.finish_input_field("item", item);
        self.value
            .queries
            .push(query.take())
            .expect("admitted nested query slot");
        Ok(index)
    }
}

async fn child<'s: 'a, 'c: 'a, 'a>(
    source: construct::Source<'s>,
    copying: &mut Copying<'c>,
    work: &mut Work<'a>,
) -> Result<Prefix<QueryPlan>> {
    let mut field = copying.begin_input_field()?;
    let output = work.submit(move |work| async move {
        let query = build(source, &mut field, work)?.await?;
        Ok::<_, SQLError>((query, field))
    })?;
    let (query, field) = construction::receive(output).await?;
    copying.finish_input_field("query", field);
    Ok(query)
}

fn build<'s: 'w, 'c: 'w, 'w: 'a, 'a>(
    source: construct::Source<'s>,
    copying: &'a mut Copying<'c>,
    work: Work<'w>,
) -> Result<construction::Read<'a, Result<Prefix<QueryPlan>>>> {
    Ok(construction::Read::new(copying.control, async move {
        let mut scope = copying.enter_plan_node()?;
        let mut input = Native {
            queries: Some(Arena::new(&scope, work.clone())),
            work,
        };
        let mut query = scope.retain_plan(crate::plan::retirement::empty_query());
        query_input(source, &mut scope, &mut input, &mut query)?.await?;
        Ok(query)
    })?)
}

fn query_input<'s: 'a, 'c: 'a, 'a, I: construct::Input<'s, 'c> + Send + 'a>(
    source: construct::Source<'s>,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
    query: &'a mut Prefix<QueryPlan>,
) -> Result<construction::Read<'a, Result<()>>> {
    Ok(construction::Read::new(
        copying.control,
        construct::construct(source, copying, input, query),
    )?)
}

impl<'s: 'a, 'c: 'a, 'a> construct::Input<'s, 'c> for Native<'a, Arena<'a>> {
    async fn ctes(
        &mut self,
        source: &'s [crate::ast::CTE],
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<Vec<CtePlan>>> {
        ctes::list_input(source, copying, self)?.await
    }

    async fn query(
        &mut self,
        source: construct::Source<'s>,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<QueryPlan>> {
        child(source, copying, &mut self.work).await
    }
}

impl<'s: 'a, 'c: 'a, 'a> block::Input<'s, 'c> for Native<'a, Arena<'a>> {
    async fn source(
        &mut self,
        source: &'s crate::ast::FromClause,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<SourcePlan>> {
        trees::source_child(source, copying, self).await
    }

    async fn finish_arena(&mut self, copying: &mut Copying<'c>) -> Result<Prefix<Vec<QueryPlan>>> {
        self.work.checkpoint().await;
        let queries =
            copying.take_arena(self.queries.take().expect("completed query arena").value)?;
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

type QueryOutput = (Prefix<QueryPlan>, RewriteMemory);
type QueryInput = ImageDecoder<QueryOutput, SQLError>;

fn replace(expression: &Expr) -> Option<usize> {
    matches!(expression, Expr::Not(_)).then_some(7)
}

fn input(
    source: Arc<Statement>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    parameters: bool,
) -> QueryInput {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        let mut driver = construction::Driver::new(control)?;
        let source = match source.as_ref() {
            Statement::Select(query) => construct::Source::Select(query),
            Statement::Values { rows } => construct::Source::Values(rows),
            _ => unreachable!("query fixture"),
        };
        let output = driver.work().submit(move |work| async move {
            let mut copying = Copying::for_rewrite(control).unwrap();
            copying.plan_retirement = Some(Context::new(&control)?);
            copying.parameters = parameters.then_some(&replace);
            let query = build(source, &mut copying, work)?.await?;
            Ok::<_, SQLError>((query, copying.into_rewrite_memory()))
        })?;
        while driver.advance().await? {
            channel.checkpoint().await?;
        }
        construction::receive(output).await
    })
    .unwrap()
}

fn advance(reader: &mut QueryInput) -> Result<Option<QueryOutput>> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(reader: &mut QueryInput) -> (QueryOutput, usize) {
    for polls in 1..1_000_000 {
        if let Some(output) = advance(reader).unwrap() {
            return (output, polls);
        }
    }
    panic!("query construction stopped progressing")
}

fn statement(sql: &str) -> Statement {
    crate::compile(sql).unwrap().remove(0)
}

#[test]
fn retained_enclosing_queries_construct_real_arenas_sets_values_and_parameter_scopes() {
    let mut sources = [
        "SELECT 1",
        "SELECT (SELECT (SELECT 1)), (SELECT 2) IN (SELECT (SELECT 3)), EXISTS (SELECT 4)",
        "SELECT (SELECT 1) FROM (VALUES ((SELECT 2))) AS v(n) WHERE (SELECT true) ORDER BY (SELECT 3) LIMIT (SELECT 4)",
        "VALUES ((SELECT 1), (SELECT (SELECT 2))), ((SELECT 3), NULL)",
        "SELECT (SELECT 1) UNION ALL SELECT (SELECT 2) ORDER BY (SELECT 3) LIMIT (SELECT 4) OFFSET (SELECT 5)",
        "(SELECT (SELECT 1) INTERSECT SELECT (SELECT 2)) EXCEPT ALL (SELECT (SELECT 3) UNION SELECT (SELECT 4))",
        "SELECT NOT ((SELECT 1) = 2), (SELECT NOT ((SELECT 3) = 4)), (SELECT 5)",
    ].into_iter().map(statement).collect::<Vec<_>>();
    sources.push(Statement::Values { rows: Vec::new() });
    sources.push(Statement::Values {
        rows: vec![
            vec![],
            vec![Expr::ScalarSubquery(Box::new(blocks::query("SELECT 8")))],
        ],
    });
    let mut implicit = blocks::query("SELECT (SELECT 1)");
    implicit.set_op =
        blocks::query("SELECT 1 UNION ALL SELECT (SELECT 2) ORDER BY (SELECT 3)").set_op;
    implicit.set_op.as_mut().unwrap().left = None;
    sources.push(Statement::Select(Box::new(implicit)));
    assert_equivalent(sources);
}

fn assert_equivalent(sources: impl IntoIterator<Item = Statement>) {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for source in sources {
        let source = Arc::new(source);
        for parameters in [false, true] {
            let control = ProductionControl::new(&native, &token, &token);
            let expected = UnifiedPlan::lower_borrowed_with_parameters(
                &source,
                &NoRegisteredAggregates,
                &control,
                parameters.then_some(&replace),
            )
            .unwrap();
            let crate::plan::MutableExecutablePlan::Query(expected) = expected else {
                unreachable!()
            };
            let ((query, memory), polls) = complete(&mut input(
                source.clone(),
                &native,
                &work,
                &token,
                &token,
                parameters,
            ));
            assert_eq!(
                serde_json::to_value(&*query).unwrap(),
                serde_json::to_value(&*expected).unwrap(),
                "parameters={parameters}"
            );
            if !parameters {
                let UnifiedPlan::Query(ordinary) = UnifiedPlan::lower(source.as_ref().clone())
                else {
                    unreachable!()
                };
                assert_eq!(
                    serde_json::to_value(&*query).unwrap(),
                    serde_json::to_value(&*ordinary).unwrap()
                );
            }
            assert!(polls > 1);
            assert_eq!(allocation_counter::measure(|| drop(query)).count_total, 0);
            drop((memory, expected));
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_enclosing_queries_release_each_pending_arena_and_set_prefix() {
    assert_prefix_retirement(statement("SELECT (SELECT 1), (SELECT 2) IN (SELECT 3) UNION ALL SELECT (SELECT 4) ORDER BY (SELECT 5) LIMIT (SELECT 6)"));
}

fn assert_prefix_retirement(source: Statement) {
    let source = Arc::new(source);
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (output, polls) = complete(&mut input(
        source.clone(),
        &native,
        &work,
        &token,
        &token,
        false,
    ));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking, false);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancel != 0 {
                if cancel == 1 {
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
            assert_eq!(
                (native.used(), work.used()),
                (37, 0),
                "prefix={prefix}, cancellation={cancel}"
            );
        }
    }
    drop(sibling);
    for bytes in [0, 1, 512, 4096, 8192, 32768] {
        let denied = MemoryBudget::new(bytes);
        let error = input(source.clone(), &denied, &work, &token, &token, false)
            .read(&mut std::io::empty())
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
}

#[test]
fn retained_enclosing_queries_bound_nested_polling_and_retire_late_failures() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(64 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            for fail in [false, true] {
                let mut source = blocks::query("SELECT 1");
                for _ in 0..512 {
                    let mut parent = blocks::query("SELECT 1");
                    parent.projections[0].expr = Expr::ScalarSubquery(Box::new(source));
                    source = parent;
                }
                if fail {
                    source.projections[0].alias = Some("reject_source_alias".into());
                }
                let result = input(
                    Arc::new(Statement::Select(Box::new(source))),
                    &native,
                    &work,
                    &token,
                    &token,
                    false,
                )
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
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
