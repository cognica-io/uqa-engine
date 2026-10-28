//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics-driven DPccp join reordering and predicate attachment.

use super::{
    BTreeMap, BTreeSet, BinaryOp, CommandPlan, ExpressionPlan, JoinExecutionStrategy,
    JoinGraphError, JoinGraphResult, JoinOrderOptimizer, JoinPredicate, JoinRelation, QueryPlan,
    RelationalPlan, ScalarExpr, ScalarFrameBound, SourcePlan, SourceStatistics, UnifiedPlan,
};

use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

use super::OptimizerResult;

const DEFAULT_JOIN_CARDINALITY: u64 = 1_000;

#[cfg(test)]
mod tests;

mod materialization;

pub(super) fn reorder_unified_plan_joins(
    plan: &mut UnifiedPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    match plan {
        UnifiedPlan::Query(query) => reorder_query_joins(query, statistics, control),
        UnifiedPlan::Command(command) => reorder_command_joins(command, statistics, control),
    }
}

fn reorder_query_joins(
    query: &mut QueryPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    for cte in &mut query.ctes {
        match &mut cte.body {
            crate::CtePlanBody::Query(query) => reorder_query_joins(query, statistics, control)?,
            crate::CtePlanBody::Command(command) => {
                reorder_command_joins(command, statistics, control)?;
            }
        }
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
                reorder_source_joins(source, &where_predicates, statistics, control)?;
            }
            for subquery in &mut block.subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            reorder_query_joins(left, statistics, control)?;
            reorder_query_joins(right, statistics, control)?;
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
    }
    Ok(())
}

fn reorder_cte_joins(
    cte: &mut crate::CtePlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    match &mut cte.body {
        crate::CtePlanBody::Query(query) => reorder_query_joins(query, statistics, control),
        crate::CtePlanBody::Command(command) => reorder_command_joins(command, statistics, control),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "traverses each command kind and its owned children"
)]
fn reorder_command_joins(
    command: &mut CommandPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    match command {
        CommandPlan::Insert(plan) => reorder_insert_joins(plan, statistics, control)?,
        CommandPlan::Update(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, &[], statistics, control)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
        CommandPlan::Delete(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, &[], statistics, control)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
        CommandPlan::Merge(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control)?;
            }
            reorder_source_joins(&mut plan.source, &[], statistics, control)?;
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
        CommandPlan::DeclareCursor { query, .. } => {
            reorder_query_joins(query, statistics, control)?;
        }
        CommandPlan::Call { args: params, .. } => {
            reorder_expression_subquery_joins(params, statistics, control)?;
        }
        CommandPlan::Explain { .. }
        | CommandPlan::CreateTableAs { .. }
        | CommandPlan::CreateMaterializedView { .. }
        | CommandPlan::CreateView { .. }
        | CommandPlan::Execute { .. }
        | CommandPlan::Prepare { .. }
        | CommandPlan::CreateTable(_)
        | CommandPlan::CreateTableIfNotExists(_)
        | CommandPlan::CreateIndex(_)
        | CommandPlan::RenameIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::RefreshMaterializedView { .. }
        | CommandPlan::AlterTable(_)
        | CommandPlan::AlterForeignTable(_)
        | CommandPlan::AlterView(_)
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::AlterSchemaOwner { .. }
        | CommandPlan::RenameSchema { .. }
        | CommandPlan::Notify { .. }
        | CommandPlan::Listen { .. }
        | CommandPlan::Unlisten { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ResetVariable { .. }
        | CommandPlan::ResetAllVariables
        | CommandPlan::SetConstraints { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Load { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::LockTable(_)
        | CommandPlan::Vacuum(_)
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::FetchCursor(_)
        | CommandPlan::CloseCursor { .. }
        | CommandPlan::CreateSequence(_)
        | CommandPlan::CreateDomain(_)
        | CommandPlan::AlterDomain(_)
        | CommandPlan::CommentType(_)
        | CommandPlan::CreateShellType(_)
        | CommandPlan::CreateEnum(_)
        | CommandPlan::CreateCompositeType(_)
        | CommandPlan::AlterEnum(_)
        | CommandPlan::AlterTypeObject(_)
        | CommandPlan::GrantType(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignWrapper(_)
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateForeignTableDefinition(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::AlterRoutine(_)
        | CommandPlan::AlterRoutineOwner(_)
        | CommandPlan::RenameRoutine(_)
        | CommandPlan::GrantRoutine(_)
        | CommandPlan::GrantTable(_)
        | CommandPlan::GrantSequence(_)
        | CommandPlan::GrantDatabase(_)
        | CommandPlan::GrantSchema(_)
        | CommandPlan::GrantRole(_)
        | CommandPlan::CreateRole(_)
        | CommandPlan::AlterRole(_)
        | CommandPlan::RenameRole(_)
        | CommandPlan::DropRole(_)
        | CommandPlan::CreateTrigger(_)
        | CommandPlan::DropTrigger(_)
        | CommandPlan::CreateRule(_)
        | CommandPlan::DropRule(_)
        | CommandPlan::DoBlock { .. }
        | CommandPlan::Empty => {}
    }
    Ok(())
}

fn reorder_insert_joins(
    plan: &mut crate::unified_plan::InsertPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    for cte in &mut plan.ctes {
        match &mut cte.body {
            crate::CtePlanBody::Query(query) => reorder_query_joins(query, statistics, control)?,
            crate::CtePlanBody::Command(command) => {
                reorder_command_joins(command, statistics, control)?;
            }
        }
    }
    if let Some(source) = &mut plan.source {
        reorder_query_joins(source, statistics, control)?;
    }
    for subquery in &mut plan.subqueries {
        reorder_query_joins(subquery, statistics, control)?;
    }
    Ok(())
}

fn reorder_expression_subquery_joins(
    expressions: &mut [ExpressionPlan],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    for expression in expressions {
        for subquery in &mut expression.subqueries {
            reorder_query_joins(subquery, statistics, control)?;
        }
    }
    Ok(())
}

fn reorder_source_joins(
    source: &mut SourcePlan,
    external_predicates: &[&ScalarExpr],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    match source {
        SourcePlan::RetainedRows(_) => {}
        SourcePlan::Join { left, right, .. } => {
            reorder_source_joins(left, &[], statistics, control)?;
            reorder_source_joins(right, &[], statistics, control)?;
        }
        SourcePlan::Subquery { body, .. } => reorder_query_joins(body, statistics, control)?,
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }

    if let Some(reordered) =
        reordered_inner_join_source(source, external_predicates, statistics, control)?
    {
        *source = reordered;
    }
    Ok(())
}

#[derive(Debug)]
struct JoinPredicateBinding {
    expression: Option<ScalarExpr>,
    qualifiers: BTreeSet<String>,
    pushable: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "join rewrite preserves predicate ownership and relation order atomically"
)]
fn reordered_inner_join_source(
    source: &mut SourcePlan,
    external_predicates: &[&ScalarExpr],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<Option<SourcePlan>> {
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
            SourcePlan::RetainedRows(_) => return Ok(None),
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
        control,
    )?;

    // SQL's comma-separated FROM form lowers to a tree of cross joins while
    // its join equalities remain in the query-block WHERE expression. Treat
    // only top-level conjunctive column equalities as join-graph edges. The
    // WHERE expression stays in place as a post-join semantic guard; adding
    // the same equality to the hash join changes evaluation shape, not rows.
    let mut inferred = Vec::new();
    for expression in external_predicates {
        for (implied, _) in implied_join_predicates(expression, &aliases, &column_owners) {
            inferred.push(implied);
        }
    }

    let mut predicates = Vec::new();
    let mut graph_predicates = Vec::new();
    for expression in expressions
        .into_iter()
        .map(std::borrow::Cow::Borrowed)
        .chain(inferred.into_iter().map(std::borrow::Cow::Owned))
    {
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
            expression: match expression {
                std::borrow::Cow::Borrowed(_) => None,
                std::borrow::Cow::Owned(expression) => Some(expression),
            },
            qualifiers,
            pushable,
        });
    }

    let selected =
        JoinOrderOptimizer::new().select_with_control(&relations, &graph_predicates, control)?;
    let mut reordered = materialization::reorder(source, &selected, &mut predicates)?;
    attach_join_predicates(&mut reordered, &mut predicates, true);
    if !predicates.is_empty() {
        return Err(JoinGraphError::InvalidPlan {
            detail: format!(
                "join reordering failed to retain {} predicate(s)",
                predicates.len()
            ),
        }
        .into());
    }
    Ok(Some(reordered))
}

fn flatten_reorderable_inner_join<'a>(
    source: &'a SourcePlan,
    atoms: &mut Vec<&'a SourcePlan>,
    predicates: &mut Vec<&'a ScalarExpr>,
) -> bool {
    match source {
        SourcePlan::RetainedRows(_) => false,
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
            ..
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
            atoms.push(source);
            true
        }
        SourcePlan::Join { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => false,
    }
}

fn collect_conjuncts<'a>(expression: &'a ScalarExpr, output: &mut Vec<&'a ScalarExpr>) {
    if let ScalarExpr::And(items) = expression {
        for item in items {
            collect_conjuncts(item, output);
        }
    } else {
        output.push(expression);
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

fn copy_local_conjunction(
    predicates: &[&ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<Produced<ScalarExpr>, uqa_sql::SQLError> {
    let mut expressions = ProductionVec::new(*control);
    expressions.reserve(predicates.len())?;
    for predicate in predicates {
        expressions.push_produced(predicate.clone_with_control(control)?)?;
    }
    let (expressions, memory) = expressions.finish()?.into_parts();
    Ok(control.finish(ScalarExpr::And(expressions), memory)?)
}

fn apply_local_filter_estimates(
    relations: &mut [JoinRelation],
    predicates: &[&ScalarExpr],
    aliases: &BTreeSet<String>,
    column_owners: &ColumnOwners,
    relation_tables: &BTreeMap<String, String>,
    source_statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    let estimator = crate::CardinalityEstimator::new();
    let mut predicates_by_alias = BTreeMap::<String, Vec<&ScalarExpr>>::new();
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
            .push(*predicate);
    }

    for relation in relations.iter_mut() {
        let Some(predicates) = predicates_by_alias.remove(&relation.alias) else {
            continue;
        };
        let conjunction;
        let predicate = if let [predicate] = predicates.as_slice() {
            *predicate
        } else {
            conjunction = copy_local_conjunction(&predicates, control)?;
            &conjunction
        };
        let statistics = crate::cardinality::RelationStatsRef {
            row_count: relation.cardinality.round() as u64,
            columns: &relation.column_stats,
        };
        let local_estimate = relation_tables
            .get(&relation.alias)
            .and_then(|table| source_statistics.local_access_estimate(table.as_str(), predicate));
        if let Some(estimate) = local_estimate {
            if !estimate.output_rows.is_finite() || estimate.output_rows < 0.0 {
                return Err(JoinGraphError::InvalidCardinality {
                    name: relation.alias.clone(),
                    rows: estimate.output_rows,
                }
                .into());
            }
            if !estimate.cost.is_finite() || estimate.cost < 0.0 {
                return Err(JoinGraphError::InvalidAccessCost {
                    name: relation.alias.clone(),
                    cost: estimate.cost,
                }
                .into());
            }
            relation.cardinality = estimate.output_rows.min(relation.cardinality).max(0.0);
            relation.access_cost = estimate.cost;
            relation.paradigm = estimate.paradigm;
            continue;
        }
        let selectivity = estimator
            .scalar_selectivity_with_statistics(predicate, statistics)
            .raw();
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
            let Some(Some((alias, _))) = column_owners.get(column.as_str()) else {
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
        ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_) => true,
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
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(recur),
        ScalarExpr::Binary { lhs, rhs, .. } => recur(lhs) && recur(rhs),
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => recur(inner),
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
        } if aliases.contains(qualifier) => Some((qualifier.clone(), column.to_string())),
        ScalarExpr::Column(column) => column_owners.get(column.as_str())?.clone(),
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
            assigned.push(predicate.expression.expect("materialized join predicate"));
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
        SourcePlan::RetainedRows(_) => {}
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
    if let Some(qualifier) = expression.reference_qualifier() {
        output.insert(qualifier.to_owned());
    }
    match expression {
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
        | ScalarExpr::CompositeRow { items, .. }
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
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => collect_scalar_qualifiers(inner, output),
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
        ScalarExpr::WindowCall {
            args, spec, filter, ..
        } => {
            for argument in args {
                collect_scalar_qualifiers(argument, output);
            }
            if let Some(filter) = filter {
                collect_scalar_qualifiers(filter, output);
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
        ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
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
