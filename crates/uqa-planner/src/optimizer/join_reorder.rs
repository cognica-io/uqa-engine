//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

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
            let where_predicates = block
                .r#where
                .as_ref()
                .map(|predicate| {
                    let mut predicates = Vec::new();
                    collect_conjuncts(predicate, &mut predicates);
                    predicates
                })
                .unwrap_or_default();
            if let Some(source) = &mut block.from {
                reorder_source_joins(source, &where_predicates, statistics)?;
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
                reorder_source_joins(source, &[], statistics)?;
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
                reorder_source_joins(source, &[], statistics)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::Merge(plan) => {
            reorder_source_joins(&mut plan.source, &[], statistics)?;
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
    external_predicates: &[ScalarExpr],
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    match source {
        SourcePlan::Join { left, right, .. } => {
            reorder_source_joins(left, &[], statistics)?;
            reorder_source_joins(right, &[], statistics)?;
        }
        SourcePlan::Subquery { body, .. } => reorder_query_joins(body, statistics)?,
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => {}
    }

    if let Some(reordered) = reordered_inner_join_source(source, external_predicates, statistics)? {
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
    external_predicates: &[ScalarExpr],
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
    let column_owners = unique_column_owners(&relations);
    apply_local_filter_selectivity(
        &mut relations,
        external_predicates,
        &aliases,
        &column_owners,
    );

    // SQL's comma-separated FROM form lowers to a tree of cross joins while
    // its join equalities remain in the query-block WHERE expression. Treat
    // only top-level conjunctive column equalities as join-graph edges. The
    // WHERE expression stays in place as a post-join semantic guard; adding
    // the same equality to the hash join changes evaluation shape, not rows.
    for expression in external_predicates {
        for (implied, _) in implied_join_predicates(expression, &aliases, &column_owners) {
            expressions.push(implied);
        }
    }

    let mut predicates = Vec::new();
    let mut graph_predicates = Vec::new();
    for expression in expressions {
        let mut qualifiers = BTreeSet::new();
        collect_scalar_qualifiers(&expression, &mut qualifiers);
        let graph_predicate = join_predicate(&expression, &aliases, &column_owners);
        if let Some(predicate) = &graph_predicate {
            qualifiers.insert(predicate.left_alias.clone());
            qualifiers.insert(predicate.right_alias.clone());
        }
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
            kind: uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross,
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

type ColumnOwners = BTreeMap<String, Option<(String, String)>>;

fn unique_column_owners(relations: &[JoinRelation]) -> ColumnOwners {
    let mut owners = ColumnOwners::new();
    for relation in relations {
        for column in relation.column_stats.keys() {
            owners
                .entry(column.clone())
                .and_modify(|owner| *owner = None)
                .or_insert_with(|| Some((relation.alias.clone(), column.clone())));
        }
    }
    owners
}

fn apply_local_filter_selectivity(
    relations: &mut [JoinRelation],
    predicates: &[ScalarExpr],
    aliases: &BTreeSet<String>,
    column_owners: &ColumnOwners,
) {
    let estimator = crate::CardinalityEstimator::new();
    for predicate in predicates {
        let mut referenced = BTreeSet::new();
        if !collect_resolved_aliases(predicate, aliases, column_owners, &mut referenced)
            || referenced.len() != 1
        {
            continue;
        }
        let Some(alias) = referenced.first() else {
            continue;
        };
        let Some(relation) = relations
            .iter_mut()
            .find(|relation| relation.alias == *alias)
        else {
            continue;
        };
        let statistics = crate::RelationStats {
            row_count: relation.cardinality.round() as u64,
            columns: relation.column_stats.clone(),
        };
        let selectivity = estimator.scalar_selectivity(predicate, &statistics).raw();
        relation.cardinality = (relation.cardinality * selectivity).max(1.0);
    }
}

fn collect_resolved_aliases(
    expression: &ScalarExpr,
    aliases: &BTreeSet<String>,
    column_owners: &ColumnOwners,
    output: &mut BTreeSet<String>,
) -> bool {
    let mut recur = |expression: &ScalarExpr| {
        collect_resolved_aliases(expression, aliases, column_owners, output)
    };
    match expression {
        ScalarExpr::Column(column) => {
            let Some(Some((alias, _))) = column_owners.get(column) else {
                return false;
            };
            output.insert(alias.clone());
            true
        }
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            if !aliases.contains(qualifier) {
                return false;
            }
            output.insert(qualifier.clone());
            true
        }
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => true,
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().all(&mut recur)
                && order_by.iter().all(|order| recur(&order.expr))
                && filter.as_deref().is_none_or(recur)
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().all(recur)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => recur(lhs) && recur(rhs),
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => recur(inner),
        ScalarExpr::Between { expr, low, high } => recur(expr) && recur(low) && recur(high),
        ScalarExpr::InList { expr, list, .. } => recur(expr) && list.iter().all(recur),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_none_or(&mut recur)
                && when
                    .iter()
                    .all(|(condition, result)| recur(condition) && recur(result))
                && else_branch.as_deref().is_none_or(recur)
        }
        ScalarExpr::Star
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}

fn join_predicate(
    expression: &ScalarExpr,
    aliases: &BTreeSet<String>,
    column_owners: &ColumnOwners,
) -> Option<JoinPredicate> {
    let ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    } = expression
    else {
        return None;
    };
    let (left_alias, left_field) = resolve_join_column(lhs, aliases, column_owners)?;
    let (right_alias, right_field) = resolve_join_column(rhs, aliases, column_owners)?;
    if left_alias == right_alias {
        return None;
    }
    Some(JoinPredicate {
        left_alias,
        right_alias,
        left_field,
        right_field,
    })
}

fn resolve_join_column(
    expression: &ScalarExpr,
    aliases: &BTreeSet<String>,
    column_owners: &ColumnOwners,
) -> Option<(String, String)> {
    match expression {
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } if aliases.contains(qualifier) => Some((qualifier.clone(), column.clone())),
        ScalarExpr::Column(column) => column_owners.get(column)?.clone(),
        _ => None,
    }
}

type JoinPredicateKey = ((String, String), (String, String));

fn join_predicate_key(predicate: &JoinPredicate) -> JoinPredicateKey {
    let left = (predicate.left_alias.clone(), predicate.left_field.clone());
    let right = (predicate.right_alias.clone(), predicate.right_field.clone());
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

/// Return equality predicates that must be true whenever `expression` is true.
/// Conjunction contributes every implied predicate; disjunction contributes
/// only predicates implied by every branch. This recognizes TPC-H Q19's
/// repeated part/lineitem equality without applying an unsafe OR pushdown.
fn implied_join_predicates(
    expression: &ScalarExpr,
    aliases: &BTreeSet<String>,
    column_owners: &ColumnOwners,
) -> Vec<(ScalarExpr, JoinPredicate)> {
    if let Some(predicate) = join_predicate(expression, aliases, column_owners) {
        return vec![(expression.clone(), predicate)];
    }
    match expression {
        ScalarExpr::And(items) => {
            let mut implied = BTreeMap::new();
            for item in items {
                for (expression, predicate) in implied_join_predicates(item, aliases, column_owners)
                {
                    implied
                        .entry(join_predicate_key(&predicate))
                        .or_insert((expression, predicate));
                }
            }
            implied.into_values().collect()
        }
        ScalarExpr::Or(items) => {
            let Some((first, rest)) = items.split_first() else {
                return Vec::new();
            };
            let mut common = implied_join_predicates(first, aliases, column_owners)
                .into_iter()
                .map(|(expression, predicate)| {
                    (join_predicate_key(&predicate), (expression, predicate))
                })
                .collect::<BTreeMap<_, _>>();
            for item in rest {
                let branch = implied_join_predicates(item, aliases, column_owners)
                    .into_iter()
                    .map(|(_, predicate)| join_predicate_key(&predicate))
                    .collect::<BTreeSet<_>>();
                common.retain(|key, _| branch.contains(key));
                if common.is_empty() {
                    break;
                }
            }
            common.into_values().collect()
        }
        _ => Vec::new(),
    }
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
