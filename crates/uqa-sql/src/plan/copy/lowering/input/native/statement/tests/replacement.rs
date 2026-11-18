//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    plan::{
        QueryLayoutUpdate, QueryMetadataUpdate, QueryPlan, RelationalPlan, SourcePlan,
        SourceReplacement,
    },
    ColumnType, RowSchema,
};

mod interruption;
mod resource;

fn statement(sql: &str) -> Arc<Statement> {
    Arc::new(crate::compile(sql).unwrap().remove(0))
}

fn construct(
    source: &Arc<Statement>,
    budget: &MemoryBudget,
    work: &MemoryBudget,
    token: &CancellationToken,
) -> RetainedExecutablePlan {
    let source = source.clone();
    let budget = budget.clone();
    let original = token.clone();
    ImageDecoder::new(work, token, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&budget, &original, channel.cancellation);
        UnifiedPlan::lower_executable_from_input(
            &source,
            &NoRegisteredAggregates,
            &control,
            &mut channel,
        )
        .await
    })
    .unwrap()
    .read(&mut std::io::empty())
    .unwrap()
}

fn query(plan: &RetainedExecutablePlan) -> &QueryPlan {
    let PlanRef::Query(query) = plan.as_plan_ref() else {
        panic!("native query");
    };
    query
}

fn block(query: &QueryPlan) -> &crate::plan::QueryBlockPlan {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block");
    };
    block
}

fn source(plan: RetainedExecutablePlan, control: &ProductionControl<'_>) -> SourceReplacement {
    let context = plan.0.context().clone();
    // The existing extraction operation transfers the complete FROM payload and its constructor records.
    let MutableExecutablePlan::Query(query) = plan.0.take() else {
        panic!("source query");
    };
    let (query, source) = query.take_source(control).unwrap();
    drop(context.retain(query));
    context.retain(source.unwrap()).into()
}

#[derive(Default)]
struct Replacements {
    query: Option<QueryMetadataUpdate>,
    at: Option<&'static str>,
    source: Option<SourceReplacement>,
    visited: Vec<String>,
}

impl<W: Work + Send> MetadataObserver<W> for Replacements {
    fn query(
        &mut self,
        query: &QueryPlan,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<Option<QueryMetadataUpdate>>> + Send {
        let selected = self.at.is_none_or(|name| {
            matches!(&query.root, RelationalPlan::QueryBlock(block)
                if matches!(block.projections.first().map(|p| &p.expr),
                    Some(ScalarExpr::Column(value)) if value == name))
        });
        std::future::ready(Ok(selected.then(|| self.query.take()).flatten()))
    }

    fn source_replacement(
        &mut self,
        _: &SourcePlan,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<Option<SourceReplacement>>> + Send {
        std::future::ready(Ok(self.source.take()))
    }

    fn projections(
        &mut self,
        projections: &[ProjectionPlan],
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<Option<ProjectionAliasUpdates>>> + Send {
        for projection in projections {
            if let ScalarExpr::Column(name) = &projection.expr {
                self.visited.push(name.to_string());
            }
        }
        std::future::ready(Ok(None))
    }
}

type Output = (RetainedExecutablePlan, Vec<String>);
type Input = ImageDecoder<Output, SQLError>;

fn input(
    mut plan: RetainedExecutablePlan,
    mut updates: Replacements,
    budget: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let budget = budget.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&budget, &original, channel.cancellation);
        plan.apply_metadata_from(&mut updates, &control, &mut channel)
            .await?;
        Ok((plan, updates.visited))
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Output>> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn layouts(
    plan: &mut RetainedExecutablePlan,
    width: usize,
    depth: usize,
    control: &ProductionControl<'_>,
) {
    let ty = (0..depth).fold(ColumnType::Text, |child, _| {
        ColumnType::Array(Box::new(child))
    });
    let schema = RowSchema::with_column_metadata_with_control(
        Some("original"),
        std::iter::repeat_n(("value", Some(&ty), Some(951)), width),
        control,
    )
    .unwrap();
    let mut updates = Replacements {
        query: Some(QueryMetadataUpdate {
            layouts: Some(QueryLayoutUpdate {
                output: Some(schema),
                source: None,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    crate::catalog::work::inline(plan.apply_metadata_from(&mut updates, control, &mut ())).unwrap();
}

#[test]
fn native_query_replacement_preserves_headers_and_moves_original_root_storage() {
    let budget = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut destination = construct(
        &statement("WITH kept AS (SELECT cte_col) SELECT old_col"),
        &budget,
        &work,
        &token,
    );
    layouts(&mut destination, 2, 2, &control);
    let expected_output = query(&destination)
        .analyzed_output
        .as_ref()
        .unwrap()
        .clone();
    let ctes = query(&destination).ctes.as_ptr();
    let MutableExecutablePlan::Query(target) = &mut *destination.0 else {
        unreachable!()
    };
    target.set_relations_bound_in_place(true, &control).unwrap();
    let replacement = construct(
        &statement("WITH discarded AS (SELECT unused_col) SELECT new_col,(SELECT nested_col)"),
        &budget,
        &work,
        &token,
    );
    let projections = block(query(&replacement)).projections.as_ptr();
    let updates = Replacements {
        query: Some(QueryMetadataUpdate {
            root: Some(replacement.try_into().unwrap()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (output, visited) = input(destination, updates, &budget, &work, &token, &token)
        .read(&mut std::io::empty())
        .unwrap();
    assert_eq!(visited, ["cte_col", "new_col", "nested_col"]);
    assert_eq!(query(&output).ctes.as_ptr(), ctes);
    assert_eq!(query(&output).ctes[0].name, "kept");
    assert!(query(&output).relations_bound);
    assert!(query(&output)
        .analyzed_output
        .as_ref()
        .unwrap()
        .shares_index(&expected_output));
    assert_eq!(block(query(&output)).projections.as_ptr(), projections);
    assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
    drop(expected_output);
    assert_eq!((budget.used(), work.used()), (0, 0));
}

#[test]
fn native_source_replacement_preserves_parent_and_visits_new_descendants() {
    let budget = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let destination = construct(
        &statement("WITH kept AS (SELECT cte_col) SELECT parent_col FROM original"),
        &budget,
        &work,
        &token,
    );
    let parent = block(query(&destination)).projections.as_ptr();
    let replacement = construct(
        &statement("SELECT 1 FROM (SELECT new_col,(SELECT nested_col)) AS replacement"),
        &budget,
        &work,
        &token,
    );
    let Some(SourcePlan::Subquery { body, .. }) = &block(query(&replacement)).from else {
        panic!("derived source");
    };
    let original_body = std::ptr::from_ref::<QueryPlan>(body);
    let updates = Replacements {
        source: Some(source(replacement, &control)),
        ..Default::default()
    };
    let (output, visited) = input(destination, updates, &budget, &work, &token, &token)
        .read(&mut std::io::empty())
        .unwrap();
    assert_eq!(visited, ["cte_col", "parent_col", "new_col", "nested_col"]);
    assert_eq!(block(query(&output)).projections.as_ptr(), parent);
    let Some(SourcePlan::Subquery { body, alias, .. }) = &block(query(&output)).from else {
        panic!("replacement source");
    };
    assert_eq!(std::ptr::from_ref::<QueryPlan>(body), original_body);
    assert_eq!(alias.as_deref(), Some("replacement"));
    assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
    assert_eq!((budget.used(), work.used()), (0, 0));
}
