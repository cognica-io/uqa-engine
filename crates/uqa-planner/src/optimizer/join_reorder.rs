//! Statistics-driven DPccp join reordering and predicate attachment.

use super::{
    BTreeMap, BTreeSet, BinaryOp, CommandPlan, JoinAlgorithm, JoinExecutionStrategy,
    JoinGraphError, JoinGraphResult, JoinOrderOptimizer, JoinOrderTree, JoinPredicate,
    JoinRelation, QueryPlan, RelationalPlan, ScalarExpr, ScalarFrameBound, SourcePlan,
    SourceStatistics, UnifiedPlan,
};

const DEFAULT_JOIN_CARDINALITY: u64 = 1_000;

pub(super) fn reorder_unified_plan_joins(
    plan: &mut UnifiedPlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    match plan {
        UnifiedPlan::Query(query) => reorder_query_joins(query, statistics),
        UnifiedPlan::Command(command) => reorder_command_joins(command, statistics),
    }
}

fn reorder_query_joins(
    query: &mut QueryPlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    for cte in &mut query.ctes {
        reorder_query_joins(&mut cte.query, statistics)?;
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                reorder_source_joins(source, statistics)?;
            }
            for subquery in &mut block.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            reorder_query_joins(left, statistics)?;
            reorder_query_joins(right, statistics)?;
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
    }
    Ok(())
}

fn reorder_command_joins(
    command: &mut CommandPlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    match command {
        CommandPlan::Insert(plan) => {
            for cte in &mut plan.ctes {
                reorder_query_joins(&mut cte.query, statistics)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_query_joins(source, statistics)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::Update(plan) => {
            for cte in &mut plan.ctes {
                reorder_query_joins(&mut cte.query, statistics)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, statistics)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::Delete(plan) => {
            for cte in &mut plan.ctes {
                reorder_query_joins(&mut cte.query, statistics)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, statistics)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::Merge(plan) => {
            reorder_source_joins(&mut plan.source, statistics)?;
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::CreateView { query, .. } | CommandPlan::CreateTableAs { query, .. } => {
            reorder_query_joins(query, statistics)?;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            reorder_unified_plan_joins(body, statistics)?;
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                for subquery in &mut expression.subqueries {
                    reorder_query_joins(subquery, statistics)?;
                }
            }
        }
        CommandPlan::CreateTable(_)
        | CommandPlan::CreateIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::AlterTable(_)
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::CreateSequence(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::DoBlock { .. } => {}
    }
    Ok(())
}

fn reorder_source_joins(
    source: &mut SourcePlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    match source {
        SourcePlan::Join { left, right, .. } => {
            reorder_source_joins(left, statistics)?;
            reorder_source_joins(right, statistics)?;
        }
        SourcePlan::Subquery { body, .. } => reorder_query_joins(body, statistics)?,
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => {}
    }

    if let Some(reordered) = reordered_inner_join_source(source, statistics)? {
        *source = reordered;
    }
    Ok(())
}

#[derive(Debug)]
struct JoinPredicateBinding {
    expression: ScalarExpr,
    qualifiers: BTreeSet<String>,
    pushable: bool,
}

fn reordered_inner_join_source(
    source: &SourcePlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<Option<SourcePlan>> {
    let mut atoms = Vec::new();
    let mut expressions = Vec::new();
    if !flatten_reorderable_inner_join(source, &mut atoms, &mut expressions)
        || !(2..=64).contains(&atoms.len())
    {
        return Ok(None);
    }

    let mut aliases = BTreeSet::new();
    let mut relations = Vec::with_capacity(atoms.len());
    for (source_id, atom) in atoms.iter().enumerate() {
        let SourcePlan::Table { name, alias } = atom else {
            return Ok(None);
        };
        let qualifier = alias
            .clone()
            .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string());
        if qualifier.is_empty() || !aliases.insert(qualifier.clone()) {
            return Ok(None);
        }
        let relation_stats = statistics.relation_statistics(name);
        let cardinality = relation_stats
            .as_ref()
            .map_or(DEFAULT_JOIN_CARDINALITY, |stats| stats.row_count)
            as f64;
        let column_stats = relation_stats.map_or_else(BTreeMap::new, |stats| stats.columns);
        let source_id = u64::try_from(source_id).map_err(|_| JoinGraphError::InvalidPlan {
            detail: format!("source index {source_id} exceeds the join plan identifier range"),
        })?;
        relations.push(JoinRelation {
            alias: qualifier,
            cardinality,
            column_stats,
            source_id,
        });
    }

    let mut predicates = Vec::new();
    let mut graph_predicates = Vec::new();
    for expression in expressions {
        let mut qualifiers = BTreeSet::new();
        collect_scalar_qualifiers(&expression, &mut qualifiers);
        let graph_predicate = join_predicate(&expression, &aliases);
        let pushable = graph_predicate.is_some();
        graph_predicates.extend(graph_predicate);
        predicates.push(JoinPredicateBinding {
            expression,
            qualifiers,
            pushable,
        });
    }

    let result = JoinOrderOptimizer::new().optimize(relations, graph_predicates)?;
    let mut reordered = materialize_join_order(result.tree, &atoms)?;
    attach_join_predicates(&mut reordered, &mut predicates, true);
    if !predicates.is_empty() {
        return Err(JoinGraphError::InvalidPlan {
            detail: format!(
                "join reordering failed to retain {} predicate(s)",
                predicates.len()
            ),
        });
    }
    Ok(Some(reordered))
}

fn flatten_reorderable_inner_join(
    source: &SourcePlan,
    atoms: &mut Vec<SourcePlan>,
    predicates: &mut Vec<ScalarExpr>,
) -> bool {
    match source {
        SourcePlan::Join {
            left,
            right,
            kind: uqa_sql::ast::JoinKind::Inner,
            on,
            lateral: false,
            strategy: _,
        } => {
            if !flatten_reorderable_inner_join(left, atoms, predicates)
                || !flatten_reorderable_inner_join(right, atoms, predicates)
            {
                return false;
            }
            if let Some(on) = on {
                collect_conjuncts(on, predicates);
            }
            true
        }
        SourcePlan::Table { .. } => {
            atoms.push(source.clone());
            true
        }
        SourcePlan::Join { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => false,
    }
}

fn collect_conjuncts(expression: &ScalarExpr, output: &mut Vec<ScalarExpr>) {
    if let ScalarExpr::And(items) = expression {
        for item in items {
            collect_conjuncts(item, output);
        }
    } else {
        output.push(expression.clone());
    }
}

fn join_predicate(expression: &ScalarExpr, aliases: &BTreeSet<String>) -> Option<JoinPredicate> {
    let ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    } = expression
    else {
        return None;
    };
    let (
        ScalarExpr::QualifiedColumn {
            qualifier: left_alias,
            column: left_field,
            ..
        },
        ScalarExpr::QualifiedColumn {
            qualifier: right_alias,
            column: right_field,
            ..
        },
    ) = (lhs.as_ref(), rhs.as_ref())
    else {
        return None;
    };
    if left_alias == right_alias || !aliases.contains(left_alias) || !aliases.contains(right_alias)
    {
        return None;
    }
    Some(JoinPredicate {
        left_alias: left_alias.clone(),
        right_alias: right_alias.clone(),
        left_field: left_field.clone(),
        right_field: right_field.clone(),
    })
}

fn materialize_join_order(
    tree: JoinOrderTree,
    atoms: &[SourcePlan],
) -> JoinGraphResult<SourcePlan> {
    match tree {
        JoinOrderTree::Scan(relation) => {
            let source_id =
                usize::try_from(relation.source_id).map_err(|_| JoinGraphError::InvalidPlan {
                    detail: format!(
                        "join source id {} exceeds the addressable source range",
                        relation.source_id
                    ),
                })?;
            atoms
                .get(source_id)
                .cloned()
                .ok_or_else(|| JoinGraphError::InvalidPlan {
                    detail: format!(
                        "join source id {source_id} is outside {} source atom(s)",
                        atoms.len()
                    ),
                })
        }
        JoinOrderTree::Inner {
            algorithm,
            left,
            right,
            ..
        } => Ok(SourcePlan::Join {
            left: Box::new(materialize_join_order(*left, atoms)?),
            right: Box::new(materialize_join_order(*right, atoms)?),
            kind: uqa_sql::ast::JoinKind::Inner,
            on: None,
            lateral: false,
            strategy: match algorithm {
                JoinAlgorithm::Hash => JoinExecutionStrategy::Hash,
            },
        }),
        JoinOrderTree::Cross { left, right } => Ok(SourcePlan::Join {
            left: Box::new(materialize_join_order(*left, atoms)?),
            right: Box::new(materialize_join_order(*right, atoms)?),
            kind: uqa_sql::ast::JoinKind::Inner,
            on: None,
            lateral: false,
            strategy: JoinExecutionStrategy::Auto,
        }),
    }
}

fn attach_join_predicates(
    source: &mut SourcePlan,
    predicates: &mut Vec<JoinPredicateBinding>,
    root: bool,
) -> BTreeSet<String> {
    let SourcePlan::Join {
        left, right, on, ..
    } = source
    else {
        return source_qualifiers(source);
    };

    let mut available = attach_join_predicates(left, predicates, false);
    available.extend(attach_join_predicates(right, predicates, false));

    let mut assigned = Vec::new();
    let mut retained = Vec::new();
    for predicate in std::mem::take(predicates) {
        if root
            || (predicate.pushable
                && !predicate.qualifiers.is_empty()
                && predicate.qualifiers.is_subset(&available))
        {
            assigned.push(predicate.expression);
        } else {
            retained.push(predicate);
        }
    }
    *predicates = retained;
    *on = match assigned.len() {
        0 => None,
        1 => assigned.pop(),
        _ => Some(ScalarExpr::And(assigned)),
    };
    available
}

fn source_qualifiers(source: &SourcePlan) -> BTreeSet<String> {
    let mut qualifiers = BTreeSet::new();
    if let SourcePlan::Table { name, alias } = source {
        qualifiers.insert(
            alias
                .clone()
                .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string()),
        );
    }
    qualifiers
}

fn collect_scalar_qualifiers(expression: &ScalarExpr, output: &mut BTreeSet<String>) {
    match expression {
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            output.insert(qualifier.clone());
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_scalar_qualifiers(argument, output);
            }
            for order in order_by {
                collect_scalar_qualifiers(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_scalar_qualifiers(filter, output);
            }
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_scalar_qualifiers(item, output);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_scalar_qualifiers(lhs, output);
            collect_scalar_qualifiers(rhs, output);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => collect_scalar_qualifiers(inner, output),
        ScalarExpr::Between { expr, low, high } => {
            collect_scalar_qualifiers(expr, output);
            collect_scalar_qualifiers(low, output);
            collect_scalar_qualifiers(high, output);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_scalar_qualifiers(expr, output);
            for item in list {
                collect_scalar_qualifiers(item, output);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_scalar_qualifiers(argument, output);
            }
            for partition in &spec.partition_by {
                collect_scalar_qualifiers(partition, output);
            }
            for order in &spec.order_by {
                collect_scalar_qualifiers(&order.expr, output);
            }
            if let Some(frame) = &spec.frame {
                collect_frame_bound_qualifiers(&frame.start, output);
                collect_frame_bound_qualifiers(&frame.end, output);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_scalar_qualifiers(base, output);
            }
            for (condition, result) in when {
                collect_scalar_qualifiers(condition, output);
                collect_scalar_qualifiers(result, output);
            }
            if let Some(branch) = else_branch {
                collect_scalar_qualifiers(branch, output);
            }
        }
        ScalarExpr::InSubquery { expr, .. } => collect_scalar_qualifiers(expr, output),
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

fn collect_frame_bound_qualifiers(bound: &ScalarFrameBound, output: &mut BTreeSet<String>) {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            collect_scalar_qualifiers(expression, output);
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
}
