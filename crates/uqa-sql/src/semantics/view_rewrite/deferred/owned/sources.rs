//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred source rewrites consume independent source/scalar owners in lexical binding order.

use super::{
    expression, namespaces, DeferredOuterRewrite, MutableQueryPlan, ProductionControl,
    RelationalPlan, RowSchema, SQLError,
};
use crate::{
    binding::{SourceBindingInput, SourceScope},
    plan::{
        MutableSourcePlan, QueryPlan, ScalarQueryArena, SourceChildSlot, SourcePlan,
        SourceScalarSlot,
    },
    production::Buffer,
};
use uqa_core::memory::Produced;

type BindSource<'a> = dyn FnMut(SourceBindingInput<'_>, &[QueryPlan], SourceScope<'_>) -> Result<RowSchema, SQLError>
    + 'a;

pub fn resolve_outer_sources_owned(
    query: MutableQueryPlan,
    outer: Option<&RowSchema>,
    bind: &mut BindSource<'_>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Err(SQLError::Internal(
            "outer source rewrites require a query block".into(),
        ));
    };
    if block.deferred_outer_rewrites.is_empty() || block.from.is_none() {
        return Ok(query);
    }
    let (query, rewrites) = query.take_deferred_rewrites(control)?;
    let mut retained = Buffer::new(control);
    for rewrite in rewrites {
        retained.push(rewrite, control)?;
    }
    let query = query.map_source_with_arena(control, |source, arena| {
        Ok(rewrite_source(
            source,
            &retained,
            arena,
            SourceScope::new(outer),
            bind,
            control,
        )?
        .0)
    })?;
    query.replace_deferred_rewrites(retained.into_iter(), control)
}

fn rewrite_source(
    mut source: MutableSourcePlan,
    rewrites: &[Produced<DeferredOuterRewrite>],
    arena: &mut ScalarQueryArena,
    scope: SourceScope<'_>,
    bind: &mut BindSource<'_>,
    control: &ProductionControl<'_>,
) -> Result<(MutableSourcePlan, RowSchema), SQLError> {
    let local = match (SourceScope {
        outer: None,
        ..scope
    })
    .input_with_control(&source, control)?
    {
        Some(local) => local,
        None => RowSchema::new_with_control(
            control.finish(Vec::new(), control.empty_reservation())?,
            control,
        )?,
    };
    let mut children = None;
    match &*source {
        SourcePlan::Join { .. } => {
            let joined = rewrite_join(source, rewrites, arena, scope, bind, control)?;
            source = joined.source;
            children = Some((joined.left, joined.right));
        }
        SourcePlan::Function { args, .. } => {
            let count = args.len();
            for rewrite in rewrites {
                for index in 0..count {
                    source = scalar(
                        source,
                        SourceScalarSlot::Argument(index),
                        rewrite,
                        &local,
                        arena,
                        control,
                    )?;
                }
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            let count = functions.len();
            for rewrite in rewrites {
                for function in 0..count {
                    let SourcePlan::FunctionGroup { functions, .. } = &*source else {
                        unreachable!("retained group")
                    };
                    for argument in 0..functions[function].args.len() {
                        source = scalar(
                            source,
                            SourceScalarSlot::GroupArgument { function, argument },
                            rewrite,
                            &local,
                            arena,
                            control,
                        )?;
                    }
                }
            }
        }
        SourcePlan::Values { rows, .. } => {
            let count = rows.len();
            for rewrite in rewrites {
                for row in 0..count {
                    let SourcePlan::Values { rows, .. } = &*source else {
                        unreachable!("retained VALUES")
                    };
                    for column in 0..rows[row].len() {
                        source = scalar(
                            source,
                            SourceScalarSlot::Value { row, column },
                            rewrite,
                            &local,
                            arena,
                            control,
                        )?;
                    }
                }
            }
        }
        SourcePlan::Subquery { .. } => {
            for rewrite in rewrites {
                let rewrite =
                    namespaces::in_namespace(rewrite, Some(&local), selection(&local), control)?;
                source = source
                    .map_query(control, |query| expression::query(&rewrite, query, control))?;
            }
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } => {}
    }
    let input = children
        .as_ref()
        .map_or(SourceBindingInput::Leaf(&source), |(left, right)| {
            SourceBindingInput::Join {
                source: &source,
                left,
                right,
            }
        });
    let schema = bind(input, arena.queries(), scope)?;
    Ok((source, schema))
}

fn scalar(
    source: MutableSourcePlan,
    slot: SourceScalarSlot,
    rewrite: &DeferredOuterRewrite,
    local: &RowSchema,
    arena: &mut ScalarQueryArena,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    let rewrite = namespaces::in_namespace(rewrite, Some(local), selection(local), control)?;
    source.map_scalar(slot, control, |scalar| {
        scalar.transform_nodes(control, &mut |_, scalar| {
            expression::node(&rewrite, scalar, arena, control)
        })
    })
}

fn selection(local: &RowSchema) -> namespaces::Selection {
    if local.columns_are_open(None) {
        namespaces::Selection::Qualified
    } else {
        namespaces::Selection::All
    }
}

struct JoinedSource {
    source: MutableSourcePlan,
    left: RowSchema,
    right: RowSchema,
}

fn rewrite_join(
    mut source: MutableSourcePlan,
    rewrites: &[Produced<DeferredOuterRewrite>],
    arena: &mut ScalarQueryArena,
    scope: SourceScope<'_>,
    bind: &mut BindSource<'_>,
    control: &ProductionControl<'_>,
) -> Result<JoinedSource, SQLError> {
    let SourcePlan::Join { kind, lateral, .. } = &*source else {
        unreachable!("joined source");
    };
    let (kind, lateral) = (*kind, *lateral);
    let mut left_schema = None;
    source = source.map_child(SourceChildSlot::Left, control, |left| {
        let (left, schema) =
            rewrite_source(left, rewrites, arena, scope.with_names(), bind, control)?;
        left_schema = Some(schema);
        Ok(left)
    })?;
    let left_schema = left_schema.expect("left source binding");
    let SourcePlan::Join { left, .. } = &*source else {
        unreachable!("retained join")
    };
    let preceding = scope.right_namespace_with_control(left, &left_schema, kind, control)?;
    let mut right_schema = None;
    source = source.map_child(SourceChildSlot::Right, control, |right| {
        let (right, schema) = rewrite_source(
            right,
            rewrites,
            arena,
            SourceScope {
                outer: scope.outer,
                preceding: Some(&preceding),
                inherited_lateral: lateral,
                retain_names: true,
            },
            bind,
            control,
        )?;
        right_schema = Some(schema);
        Ok(right)
    })?;
    let right_schema = right_schema.expect("right source binding");
    if matches!(&*source, SourcePlan::Join { on: Some(_), .. }) {
        let local = RowSchema::join_with_control(&left_schema, &right_schema, [], control)?;
        for rewrite in rewrites {
            source = scalar(
                source,
                SourceScalarSlot::JoinOn,
                rewrite,
                &local,
                arena,
                control,
            )?;
        }
    }
    Ok(JoinedSource {
        source,
        left: left_schema,
        right: right_schema,
    })
}
