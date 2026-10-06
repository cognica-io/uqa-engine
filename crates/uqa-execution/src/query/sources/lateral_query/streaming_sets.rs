//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deliver correlated UNION ALL rows directly to the enclosing consumer.

use super::{
    execute_lateral_subquery_output_inner, CteScope, QueryOutput, QueryOutputMode, QueryPlan,
    RelationalPlan, SQLError, SQLParam, SourceContext,
};
use crate::query::{
    output::QueryRows,
    statement::consumer::{QueryConsumerFactory, SetOperationConsumerFactory},
};
use std::rc::Rc;

pub(super) fn try_stream_union<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    outer: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output: &QueryOutputMode<'_>,
) -> Result<Option<QueryOutput>, SQLError> {
    let RelationalPlan::SetOp {
        kind: uqa_sql::ast::SetOpKind::Union,
        all: true,
        left,
        right,
        order_by,
        limit,
        with_ties: false,
        offset,
        subqueries,
    } = &plan.root
    else {
        return Ok(None);
    };
    let QueryOutputMode::RowConsumer(downstream) = output else {
        return Ok(None);
    };
    if !order_by.is_empty() {
        return Ok(None);
    }
    let schema = crate::query::binding::bind_query_plan_schema(
        context.ctes.routines,
        plan,
        params,
        ctes,
        Some(&outer.schema),
    )?;
    let (offset, limit) = {
        let scope = ctes.enter_scalar_subqueries(subqueries);
        let resolve = |expression, name| {
            crate::query::relational::row_count::resolve_limit_offset_with_ctes(
                expression,
                context.relational,
                params,
                name,
                &scope,
            )
        };
        (
            resolve(offset.as_deref(), "OFFSET")?.unwrap_or(0),
            resolve(limit.as_deref(), "LIMIT")?,
        )
    };
    let crate::query::statement::consumer::QueryOutputMode::RowConsumer(factory) =
        crate::query::statement::consumer::QueryOutputMode::<S>::physical_consumer(Rc::clone(
            downstream,
        ))
    else {
        unreachable!()
    };
    let factory = Rc::new(SetOperationConsumerFactory::new(
        factory,
        schema.clone(),
        offset,
        limit,
    ));
    if factory.stopped() {
        downstream.begin(schema.columns(), &schema)?;
    } else {
        let consumer = Rc::clone(&factory).bind(None)?;
        execute_lateral_subquery_output_inner(
            context,
            left,
            outer,
            params,
            ctes,
            QueryOutputMode::RowConsumer(Rc::clone(&consumer)),
        )?;
        if !factory.stopped() {
            execute_lateral_subquery_output_inner(
                context,
                right,
                outer,
                params,
                ctes,
                QueryOutputMode::RowConsumer(consumer),
            )?;
        }
    }
    Ok(Some(QueryOutput {
        columns: schema.columns().to_vec(),
        column_types: schema.column_types().to_vec(),
        internal_columns: schema.columns().to_vec(),
        internal_types: schema.column_types().to_vec(),
        rows: QueryRows::Rows {
            named: Vec::new(),
            positional: None,
        },
    }))
}
