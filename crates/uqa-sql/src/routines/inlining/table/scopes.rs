//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture caller attributes structurally before entering a routine's lexical scopes.

use std::collections::BTreeSet;

use crate::{
    ast::{InternalRelationId, JoinKind},
    plan::{
        AccessPathPlan, ComputePlan, JoinExecutionStrategy, ProjectionPlan, QueryBlockPlan,
        QueryPlan, RelationalPlan, SourcePlan,
    },
    ColumnType, SQLError, ScalarExpr,
};

pub(super) fn substitute(
    mut query: Box<QueryPlan>,
    arguments: &[ScalarExpr],
    width: usize,
) -> Result<Box<QueryPlan>, SQLError> {
    let relation = InternalRelationId::allocate();
    let mut captures = Vec::new();
    let mut invalid = false;
    query.rewrite_scalar_expressions(&mut |node| {
        let ScalarExpr::Param(index) = node else {
            return;
        };
        let Some(argument) = index.checked_sub(1).and_then(|index| arguments.get(index)) else {
            invalid = true;
            return;
        };
        let mut argument = argument.clone();
        crate::plan::rewrite_scalar_expression(&mut argument, &mut |reference| {
            if matches!(
                reference,
                ScalarExpr::Column(_)
                    | ScalarExpr::QualifiedColumn { .. }
                    | ScalarExpr::QualifiedStar(_)
                    | ScalarExpr::Position(_)
            ) {
                let position = captures
                    .iter()
                    .position(|capture| capture == reference)
                    .unwrap_or_else(|| {
                        captures.push(reference.clone());
                        captures.len() - 1
                    });
                *reference = ScalarExpr::InternalColumn(relation.column(position));
            }
        });
        *node = argument;
    });
    if invalid {
        return Err(SQLError::Internal(
            "SQL body parameter is outside its invocation".into(),
        ));
    }
    if captures.is_empty() {
        return Ok(query);
    }
    // Capture only attributes, never argument expressions: unused and conditional argument evaluations keep their original demand and error ordering. The inner body cannot shadow opaque attribute identities.
    let source = SourcePlan::Values {
        lateral: false,
        internal_column_types: vec![None; captures.len()],
        internal_column_collations: Vec::new(),
        rows: vec![captures],
        alias: None,
        column_aliases: Vec::new(),
        internal_relation: Some(relation),
    };
    let mut wrapper = project(query, width);
    let RelationalPlan::QueryBlock(block) = &mut wrapper.root else {
        unreachable!()
    };
    block.from = Some(SourcePlan::Join {
        comma: false,
        left: Box::new(source),
        right: Box::new(block.from.take().expect("projected query source")),
        kind: JoinKind::Cross,
        on: None,
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral: true,
        strategy: JoinExecutionStrategy::Auto,
    });
    Ok(Box::new(wrapper))
}

pub(super) fn coerce_result(
    mut query: QueryPlan,
    targets: &[ColumnType],
    sources: &[Option<ColumnType>],
) -> QueryPlan {
    if targets
        .iter()
        .zip(sources)
        .all(|(target, source)| source.as_ref() == Some(target))
    {
        return query;
    }
    let needs_wrapper = match &query.root {
        RelationalPlan::QueryBlock(block) => {
            block.distinct
                || !block.distinct_on.is_empty()
                || !block.order_by.is_empty()
                || !block.group_by.is_empty()
                || !block.grouping_sets.is_empty()
        }
        _ => true,
    };
    if needs_wrapper {
        query = project(Box::new(query), targets.len());
    }
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    for ((projection, target), source) in block.projections.iter_mut().zip(targets).zip(sources) {
        if source.as_ref() != Some(target) {
            projection.expr = ScalarExpr::Cast {
                implicit: true,
                expr: Box::new(projection.expr.clone()),
                ty: target.catalog_name(),
            };
        }
    }
    query
}

fn project(body: Box<QueryPlan>, width: usize) -> QueryPlan {
    let columns = (1..=width)
        .map(|index| format!("column{index}"))
        .collect::<Vec<_>>();
    QueryPlan {
        relations_bound: body.relations_bound,
        ctes: Vec::new(),
        root: RelationalPlan::QueryBlock(Box::new(QueryBlockPlan {
            projections: columns
                .iter()
                .map(|name| ProjectionPlan {
                    expr: ScalarExpr::Column(name.clone()),
                    alias: Some(name.clone()),
                })
                .collect(),
            from: Some(SourcePlan::Subquery {
                lateral: false,
                body,
                alias: None,
                column_aliases: columns,
            }),
            windows: Vec::new(),
            privilege_columns: BTreeSet::default(),
            r#where: None,
            compute: ComputePlan::Project,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            group_distinct: false,
            having: None,
            order_by: Vec::new(),
            limit: None,
            with_ties: false,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            subqueries: Vec::new(),
            access: AccessPathPlan::Row,
            locking: Vec::new(),
        })),
    }
}
