//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics-driven DPccp join reordering and predicate attachment.

use super::{
    BTreeMap, BTreeSet, BinaryOp, CommandPlan, ExpressionPlan, JoinAlgorithm,
    JoinExecutionStrategy, JoinGraphError, JoinGraphResult, JoinOrderOptimizer, JoinOrderTree,
    JoinPredicate, JoinRelation, QueryPlan, RelationalPlan, ScalarExpr, ScalarFrameBound,
    SourcePlan, SourceStatistics, UnifiedPlan,
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
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            reorder_query_joins(query, statistics)?;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            reorder_unified_plan_joins(body, statistics)?;
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            reorder_expression_subquery_joins(params, statistics)?;
        }
        CommandPlan::CreateTable(_)
        | CommandPlan::CreateIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::RefreshMaterializedView { .. }
        | CommandPlan::AlterTable(_)
        | CommandPlan::AlterForeignTable(_)
        | CommandPlan::AlterView(_)
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ResetVariable { .. }
        | CommandPlan::ResetAllVariables
        | CommandPlan::SetConstraints { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Load { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::Vacuum(_)
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::FetchCursor(_)
        | CommandPlan::CloseCursor { .. }
        | CommandPlan::CreateSequence(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::AlterRoutine(_)
        | CommandPlan::AlterRoutineOwner(_)
        | CommandPlan::GrantRoutine(_)
        | CommandPlan::GrantTable(_)
        | CommandPlan::GrantSequence(_)
        | CommandPlan::GrantDatabase(_)
        | CommandPlan::GrantSchema(_)
        | CommandPlan::GrantRole(_)
        | CommandPlan::CreateRole(_)
        | CommandPlan::AlterRole(_)
        | CommandPlan::DropRole(_)
        | CommandPlan::CreateTrigger(_)
        | CommandPlan::DropTrigger(_)
        | CommandPlan::CreateRule(_)
        | CommandPlan::DropRule(_)
        | CommandPlan::DoBlock { .. } => {}
    }
    Ok(())
}

fn reorder_expression_subquery_joins(
    expressions: &mut [ExpressionPlan],
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    for expression in expressions {
        for subquery in &mut expression.subqueries {
            reorder_query_joins(subquery, statistics)?;
        }
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
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
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

#[expect(
    clippy::too_many_lines,
    reason = "join rewrite preserves predicate ownership and relation order atomically"
)]
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
    let mut relation_tables = BTreeMap::new();
    for (source_id, atom) in atoms.iter().enumerate() {
        let (qualifier, cardinality, column_stats, access_cost, paradigm) = match atom {
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                ..
            } => {
                let qualifier = alias.clone().unwrap_or_else(|| qualifier.clone());
                relation_tables.insert(qualifier.clone(), name.clone());
                let relation_stats = statistics.relation_statistics(name);
                let cardinality = relation_stats
                    .as_ref()
                    .map_or(DEFAULT_JOIN_CARDINALITY, |stats| stats.row_count)
                    as f64;
                let column_stats = relation_stats.map_or_else(BTreeMap::new, |stats| stats.columns);
                let access_cost = crate::CostEstimator::default()
                    .estimate_unary(crate::OperatorKind::TableScan, cardinality)
                    .total();
                (
                    qualifier,
                    cardinality,
                    column_stats,
                    access_cost,
                    crate::AccessParadigm::Relational,
                )
            }
            SourcePlan::Function { .. } => {
                let Some(estimate) = statistics.source_access_estimate(atom) else {
                    return Ok(None);
                };
                (
                    atom.visible_qualifier()
                        .expect("a function source always has a visible qualifier")
                        .to_string(),
                    estimate.output_rows,
                    BTreeMap::new(),
                    estimate.cost,
                    estimate.paradigm,
                )
            }
            SourcePlan::Join { .. }
            | SourcePlan::Values { .. }
            | SourcePlan::FunctionGroup { .. }
            | SourcePlan::Subquery { .. } => return Ok(None),
        };
        if qualifier.is_empty() || !aliases.insert(qualifier.clone()) {
            return Ok(None);
        }
        let source_id = u64::try_from(source_id).map_err(|_| JoinGraphError::InvalidPlan {
            detail: format!("source index {source_id} exceeds the join plan identifier range"),
        })?;
        relations.push(JoinRelation {
            alias: qualifier,
            cardinality,
            column_stats,
            access_cost,
            paradigm,
            source_id,
        });
    }
    let column_owners = unique_column_owners(&relations);
    apply_local_filter_estimates(
        &mut relations,
        external_predicates,
        &aliases,
        &column_owners,
        &relation_tables,
        statistics,
    )?;

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
            using: None,
            natural: false,
            alias: None,
            column_aliases,
            lateral: false,
            strategy: _,
        } => {
            if !column_aliases.is_empty() {
                return false;
            }
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
        SourcePlan::Table { .. } | SourcePlan::Function { .. } => {
            atoms.push(source.clone());
            true
        }
        SourcePlan::Join { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::FunctionGroup { .. }
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

fn apply_local_filter_estimates(
    relations: &mut [JoinRelation],
    predicates: &[ScalarExpr],
    aliases: &BTreeSet<String>,
    column_owners: &ColumnOwners,
    relation_tables: &BTreeMap<String, String>,
    source_statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    let estimator = crate::CardinalityEstimator::new();
    let mut predicates_by_alias = BTreeMap::<String, Vec<ScalarExpr>>::new();
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
        predicates_by_alias
            .entry(alias.clone())
            .or_default()
            .push(predicate.clone());
    }

    for relation in relations.iter_mut() {
        let Some(predicates) = predicates_by_alias.remove(&relation.alias) else {
            continue;
        };
        let predicate = match predicates.as_slice() {
            [predicate] => predicate.clone(),
            _ => ScalarExpr::And(predicates),
        };
        let statistics = crate::RelationStats {
            row_count: relation.cardinality.round() as u64,
            columns: relation.column_stats.clone(),
        };
        let local_estimate = relation_tables
            .get(&relation.alias)
            .and_then(|table| source_statistics.local_access_estimate(table.as_str(), &predicate));
        if let Some(estimate) = local_estimate {
            if !estimate.output_rows.is_finite() || estimate.output_rows < 0.0 {
                return Err(JoinGraphError::InvalidCardinality {
                    name: relation.alias.clone(),
                    rows: estimate.output_rows,
                });
            }
            if !estimate.cost.is_finite() || estimate.cost < 0.0 {
                return Err(JoinGraphError::InvalidAccessCost {
                    name: relation.alias.clone(),
                    cost: estimate.cost,
                });
            }
            relation.cardinality = estimate.output_rows.min(relation.cardinality).max(0.0);
            relation.access_cost = estimate.cost;
            relation.paradigm = estimate.paradigm;
            continue;
        }
        let selectivity = estimator.scalar_selectivity(&predicate, &statistics).raw();
        relation.cardinality = (relation.cardinality * selectivity).max(0.0);
        relation.access_cost += crate::CostEstimator::default()
            .estimate_unary(crate::OperatorKind::Filter, statistics.row_count as f64)
            .total();
    }
    Ok(())
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
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(recur),
        ScalarExpr::Binary { lhs, rhs, .. } => recur(lhs) && recur(rhs),
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
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
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
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
            using: None,
            natural: false,
            alias: None,
            column_aliases: Vec::new(),
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
            using: None,
            natural: false,
            alias: None,
            column_aliases: Vec::new(),
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
    match source {
        SourcePlan::Join {
            alias: Some(alias), ..
        } => {
            qualifiers.insert(alias.clone());
        }
        SourcePlan::Join { .. } => {}
        SourcePlan::Table { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Subquery { .. } => {
            if let Some(qualifier) = source.visible_qualifier() {
                qualifiers.insert(qualifier.to_string());
            }
        }
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
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                collect_scalar_qualifiers(item, output);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_scalar_qualifiers(lhs, output);
            collect_scalar_qualifiers(rhs, output);
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
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
        ScalarExpr::QualifiedStar(qualifier) => {
            output.insert(qualifier.clone());
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
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
