//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    construct, construction, AggregateClassifier, Arena, Copying, Native, Prefix, Result, SQLError,
    Work,
};
use crate::plan::{copy::lowering::mutations as constructor, CommandPlan};

pub(super) async fn child<'s: 'a, 'c: 'a, 'a>(
    source: constructor::Source<'s>,
    copying: &mut Copying<'c>,
    work: &mut Work<'a>,
    aggregates: &'a (dyn AggregateClassifier + Sync),
) -> Result<Prefix<CommandPlan>> {
    let mut field = copying.begin_input_field()?;
    let output = work.submit(move |work| async move {
        let command = build(source, &mut field, work, aggregates)?.await?;
        Ok::<_, SQLError>((command, field))
    })?;
    let (command, field) = construction::receive(output).await?;
    copying.finish_input_field("value0", field);
    Ok(command)
}

fn build<'s: 'w, 'c: 'w, 'w: 'a, 'a>(
    source: constructor::Source<'s>,
    copying: &'a mut Copying<'c>,
    work: Work<'w>,
    aggregates: &'w (dyn AggregateClassifier + Sync),
) -> Result<construction::Read<'a, Result<Prefix<CommandPlan>>>> {
    Ok(construction::Read::new(copying.control, async move {
        let mut scope = copying.enter_plan_node()?;
        let mut input = Native {
            aggregates,
            queries: Some(Arena::new(&scope, work.clone(), aggregates)),
            work,
        };
        let result = mutation_input(source, &mut scope, &mut input)?.await;
        result
    })?)
}

fn mutation_input<'s: 'a, 'c: 'a, 'a, I: construct::Input<'s, 'c> + Send + 'a>(
    source: constructor::Source<'s>,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Prefix<CommandPlan>>>> {
    Ok(construction::Read::new(
        copying.control,
        constructor::construct(source, copying, input),
    )?)
}
