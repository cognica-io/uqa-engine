//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    child, construct, construction, Arena, Copying, Native, Prefix, QueryPlan, Result, SQLError,
    SelectStmt, SourcePlan,
};
use crate::{ast::FromClause, plan::copy::lowering::source};

pub(super) async fn source_child<'s: 'a, 'c: 'a, 'a>(
    source: &'s FromClause,
    copying: &mut Copying<'c>,
    input: &mut Native<'a, Arena<'a>>,
) -> Result<Prefix<SourcePlan>> {
    let mut field = copying.begin_input_field()?;
    let queries = input.queries.take();
    let aggregates = input.aggregates;
    let output = input.work.submit(move |work| async move {
        let mut input = Native {
            aggregates,
            work,
            queries,
        };
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
        child(
            construct::Source::Select(source),
            copying,
            &mut self.work,
            self.aggregates,
        )
        .await
    }
}
