//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE materialization, EXPLAIN, VALUES, and SELECT-without-FROM execution.

use super::*;

pub(in crate::sql) fn materialize_plan_ctes(
    engine: &Engine,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<(), SQLError> {
    materialize_plan_ctes_with_filters(engine, plans, params, ctes, &BTreeMap::new())
}

pub(in crate::sql) fn materialize_plan_ctes_with_filters<'a>(
    engine: &Engine,
    plans: impl IntoIterator<Item = &'a CtePlan>,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    let plans = order_cte_plans(plans.into_iter().collect())?;
    for plan in plans {
        if cte_references_own_name(plan) {
            let rows = {
                let mut cte_scope = ctes.enter_lock_identity_emission(false);
                materialize_recursive_cte(
                    engine,
                    plan,
                    params,
                    &mut cte_scope,
                    output_filters.get(&plan.name),
                )?
            };
            ctes.insert_shared(plan.name.clone(), rows);
            continue;
        }

        let outer_row = ctes.row_lock_outer_row().cloned();
        let result = {
            let mut cte_scope = ctes.enter_lock_identity_emission(false);
            if let Some(outer_row) = outer_row.as_ref() {
                execute_lateral_subquery_output(engine, &plan.query, outer_row, params, &cte_scope)?
            } else {
                execute_query_plan_output(
                    engine,
                    &plan.query,
                    params,
                    &mut cte_scope,
                    QueryOutputMode::SharedSpill,
                )?
            }
        };
        let mut columns = result.columns.clone();
        let source_columns = result.internal_columns.clone();
        let mut operator = result.into_operator();
        if !plan.columns.is_empty() {
            let renamed_columns = columns
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    plan.columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| source.clone())
                })
                .collect::<Vec<_>>();
            let mapping = source_columns
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    let output = renamed_columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| source.clone());
                    (output, index)
                })
                .collect();
            columns = renamed_columns;
            operator = Box::new(uqa_execution::ColumnSelection::with_positions(
                operator, mapping,
            ));
        }
        let identity = operator
            .row_schema()
            .columns()
            .iter()
            .cloned()
            .enumerate()
            .map(|(position, column)| (column, position))
            .collect();
        operator = Box::new(
            uqa_execution::ColumnSelection::with_positions(operator, identity)
                .discarding_lock_origins(),
        );
        let materialized =
            collect_query_operator(engine, columns, operator, QueryOutputMode::SharedSpill)?;
        let QueryRows::SharedSpill(materialized) = materialized.rows else {
            return Err(SQLError::Internal(
                "CTE spill collector returned in-memory rows".into(),
            ));
        };
        ctes.insert_shared(plan.name.clone(), materialized);
    }
    Ok(())
}

/// Return the CTEs whose query results can be reached from this query root. `PostgreSQL` does not evaluate an unreferenced SELECT CTE. References are resolved through nested query scopes so a shadowing inner CTE does not make an outer CTE reachable, while a reachable inner CTE can still depend on an outer one.
pub(in crate::sql) fn reachable_plan_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let targets = plan
        .ctes
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    if targets.is_empty() {
        return BTreeSet::new();
    }

    let mut reachable = BTreeSet::new();
    collect_target_cte_references_from_root(&plan.root, &targets, &BTreeSet::new(), &mut reachable);

    let mut expanded = BTreeSet::new();
    loop {
        let pending = plan
            .ctes
            .iter()
            .enumerate()
            .filter(|(_, cte)| reachable.contains(&cte.name) && !expanded.contains(&cte.name))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        for (index, cte) in pending {
            expanded.insert(cte.name.clone());
            let visible_dependencies = if cte.recursive {
                targets.clone()
            } else {
                plan.ctes[..index]
                    .iter()
                    .map(|dependency| dependency.name.clone())
                    .collect::<BTreeSet<_>>()
            };
            collect_target_cte_references_from_nested_query(
                &cte.query,
                &visible_dependencies,
                &BTreeSet::new(),
                &mut reachable,
            );
        }
    }
    reachable
}

pub(in crate::sql) fn cte_references_own_name(cte: &CtePlan) -> bool {
    let targets = BTreeSet::from([cte.name.clone()]);
    let mut references = BTreeSet::new();
    collect_target_cte_references_from_nested_query(
        &cte.query,
        &targets,
        &BTreeSet::new(),
        &mut references,
    );
    references.contains(&cte.name)
}

pub(in crate::sql) fn ordered_plan_ctes(plan: &QueryPlan) -> Result<Vec<&CtePlan>, SQLError> {
    order_cte_plans(plan.ctes.iter().collect())
}

fn order_cte_plans(plans: Vec<&CtePlan>) -> Result<Vec<&CtePlan>, SQLError> {
    if !plans.iter().any(|cte| cte.recursive) {
        return Ok(plans);
    }
    let targets = plans
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    let dependencies = plans
        .iter()
        .map(|cte| {
            let mut references = BTreeSet::new();
            collect_target_cte_references_from_nested_query(
                &cte.query,
                &targets,
                &BTreeSet::new(),
                &mut references,
            );
            references.remove(&cte.name);
            references
        })
        .collect::<Vec<_>>();
    let mut emitted = BTreeSet::new();
    let mut ordered = Vec::with_capacity(plans.len());
    let mut remaining = (0..plans.len()).collect::<BTreeSet<_>>();
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .copied()
            .find(|index| dependencies[*index].is_subset(&emitted));
        let Some(index) = ready else {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "mutual recursion between WITH items is not implemented".into(),
            });
        };
        remaining.remove(&index);
        emitted.insert(plans[index].name.clone());
        ordered.push(plans[index]);
    }
    Ok(ordered)
}

/// Return reachable CTEs with exactly one syntactic reference in the owning query tree. Counting references outside their lexical visibility can only make this set more conservative, never cause a multiply referenced CTE to be streamed as a single-consumer input.
pub(in crate::sql) fn single_reference_plan_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let targets = plan
        .ctes
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    let mut counts = targets
        .iter()
        .map(|name| (name.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    count_plan_cte_references(plan, &targets, &mut counts);
    counts
        .into_iter()
        .filter_map(|(name, count)| (count == 1).then_some(name))
        .collect()
}

fn count_plan_cte_references(
    plan: &QueryPlan,
    targets: &BTreeSet<String>,
    counts: &mut BTreeMap<String, usize>,
) {
    for cte in &plan.ctes {
        count_plan_cte_references(&cte.query, targets, counts);
    }
    count_relational_cte_references(&plan.root, targets, counts);
}

fn count_relational_cte_references(
    plan: &RelationalPlan,
    targets: &BTreeSet<String>,
    counts: &mut BTreeMap<String, usize>,
) {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                count_source_cte_references(source, targets, counts);
            }
            for subquery in &block.subqueries {
                count_plan_cte_references(subquery, targets, counts);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            count_plan_cte_references(left, targets, counts);
            count_plan_cte_references(right, targets, counts);
            for subquery in subqueries {
                count_plan_cte_references(subquery, targets, counts);
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                count_plan_cte_references(subquery, targets, counts);
            }
        }
    }
}

fn count_source_cte_references(
    source: &SourcePlan,
    targets: &BTreeSet<String>,
    counts: &mut BTreeMap<String, usize>,
) {
    match source {
        SourcePlan::Table { name, .. } => {
            if targets.contains(name) {
                *counts.entry(name.clone()).or_default() += 1;
            }
        }
        SourcePlan::Join { left, right, .. } => {
            count_source_cte_references(left, targets, counts);
            count_source_cte_references(right, targets, counts);
        }
        SourcePlan::Subquery { body, .. } => {
            count_plan_cte_references(body, targets, counts);
        }
        SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

fn collect_target_cte_references_from_root(
    root: &RelationalPlan,
    targets: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    references: &mut BTreeSet<String>,
) {
    match root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_target_cte_references_from_source(source, targets, shadowed, references);
            }
            for subquery in &block.subqueries {
                collect_target_cte_references_from_nested_query(
                    subquery, targets, shadowed, references,
                );
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            collect_target_cte_references_from_nested_query(left, targets, shadowed, references);
            collect_target_cte_references_from_nested_query(right, targets, shadowed, references);
            for subquery in subqueries {
                collect_target_cte_references_from_nested_query(
                    subquery, targets, shadowed, references,
                );
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                collect_target_cte_references_from_nested_query(
                    subquery, targets, shadowed, references,
                );
            }
        }
    }
}

fn collect_target_cte_references_from_source(
    source: &SourcePlan,
    targets: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    references: &mut BTreeSet<String>,
) {
    match source {
        SourcePlan::Table { name, .. } if targets.contains(name) && !shadowed.contains(name) => {
            references.insert(name.clone());
        }
        SourcePlan::Join { left, right, .. } => {
            collect_target_cte_references_from_source(left, targets, shadowed, references);
            collect_target_cte_references_from_source(right, targets, shadowed, references);
        }
        SourcePlan::Subquery { body, .. } => {
            collect_target_cte_references_from_nested_query(body, targets, shadowed, references);
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

fn collect_target_cte_references_from_nested_query(
    plan: &QueryPlan,
    targets: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    references: &mut BTreeSet<String>,
) {
    let local_reachable = reachable_plan_cte_names(plan);
    let mut root_shadowed = shadowed.clone();
    root_shadowed.extend(
        plan.ctes
            .iter()
            .map(|cte| cte.name.clone())
            .filter(|name| targets.contains(name)),
    );
    collect_target_cte_references_from_root(&plan.root, targets, &root_shadowed, references);
    let recursive_scope = plan.ctes.iter().any(|cte| cte.recursive).then(|| {
        plan.ctes
            .iter()
            .map(|cte| cte.name.clone())
            .collect::<BTreeSet<_>>()
    });
    let mut preceding = BTreeSet::new();
    for cte in &plan.ctes {
        if local_reachable.contains(&cte.name) {
            let mut definition_shadowed = shadowed.clone();
            if let Some(recursive_scope) = recursive_scope.as_ref() {
                definition_shadowed.extend(
                    recursive_scope
                        .iter()
                        .filter(|name| targets.contains(*name))
                        .cloned(),
                );
            } else {
                definition_shadowed.extend(
                    preceding
                        .iter()
                        .filter(|name| targets.contains(*name))
                        .cloned(),
                );
            }
            collect_target_cte_references_from_nested_query(
                &cte.query,
                targets,
                &definition_shadowed,
                references,
            );
        }
        preceding.insert(cte.name.clone());
    }
}

/// Render the inner statement as an EXPLAIN-style, single-column `plan`
/// result with one row per line.
pub(in crate::sql) struct ExplainAnalysis {
    pub(in crate::sql) elapsed: std::time::Duration,
    pub(in crate::sql) rows: u64,
    pub(in crate::sql) affected_rows: u64,
}

pub(in crate::sql) fn run_explain(
    body: &UnifiedPlan,
    verbose: bool,
    format: Option<&str>,
    analysis: Option<&ExplainAnalysis>,
) -> Result<SQLResult, SQLError> {
    let mut plan_text = match body {
        UnifiedPlan::Query(query) => format_query_plan(query),
        UnifiedPlan::Command(command) => format!("{}\n  {command:#?}", command.name()),
    };
    if verbose {
        plan_text.push_str("\n  verbose=true");
        write!(plan_text, "\n  physical_plan={body:#?}")
            .map_err(|error| SQLError::Internal(format!("format EXPLAIN plan: {error}")))?;
    }
    if let Some(analysis) = analysis {
        let _ = write!(
            plan_text,
            "\n  actual_rows={}\n  affected_rows={}\n  execution_time_ms={:.3}",
            analysis.rows,
            analysis.affected_rows,
            analysis.elapsed.as_secs_f64() * 1_000.0
        );
    }

    let format = format.unwrap_or("text").to_ascii_lowercase();
    if format == "json" {
        let payload = serde_json::json!({
            "Plan": plan_text.lines().collect::<Vec<_>>(),
            "Analyze": analysis.is_some(),
            "Actual Rows": analysis.map(|value| value.rows),
            "Affected Rows": analysis.map(|value| value.affected_rows),
            "Execution Time (ms)": analysis.map(|value| value.elapsed.as_secs_f64() * 1_000.0),
        });
        let mut row = ResultRow::new();
        row.insert("plan".to_string(), Value::Str(payload.to_string()));
        return Ok(SQLResult {
            columns: vec!["plan".to_string()],
            column_types: vec![Some(uqa_sql::ColumnType::Text)],
            rows: vec![row],
            positional_rows: None,
            affected_rows: 0,
        });
    }
    if format != "text" {
        return Err(SQLError::Unsupported(format!(
            "EXPLAIN format `{format}` is not supported; expected TEXT or JSON"
        )));
    }
    let mut rows: Vec<ResultRow> = Vec::new();
    for line in plan_text.split('\n') {
        let mut r = ResultRow::new();
        r.insert("plan".to_string(), Value::Str(line.to_string()));
        rows.push(r);
    }
    Ok(SQLResult {
        columns: vec!["plan".to_string()],
        column_types: vec![Some(uqa_sql::ColumnType::Text)],
        rows,
        positional_rows: None,
        affected_rows: 0,
    })
}

pub(in crate::sql) fn format_query_plan(plan: &QueryPlan) -> String {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => format_select_plan(block),
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => format!(
            "SetOp\n  kind={kind:?}\n  all={all}\n  left=({})\n  right=({})\n  order_by={}\n  limit={}\n  offset={}",
            format_query_plan(left).replace('\n', "\n    "),
            format_query_plan(right).replace('\n', "\n    "),
            order_by.len(),
            limit
                .as_deref()
                .map_or_else(|| "none".into(), explain_int_expr),
            offset
                .as_deref()
                .map_or_else(|| "none".into(), explain_int_expr),
        ),
        RelationalPlan::Values { rows, .. } => format!("Values\n  rows={}", rows.len()),
    }
}

pub(in crate::sql) fn format_select_plan(stmt: &QueryBlockPlan) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "Select");
    if !stmt.projections.is_empty() {
        let _ = writeln!(s, "  projections={}", stmt.projections.len());
    }
    if let Some(from) = &stmt.from {
        let _ = writeln!(s, "  from={from:?}");
    }
    if stmt.r#where.is_some() {
        let _ = writeln!(s, "  where=<expr>");
    }
    if !stmt.group_by.is_empty() {
        let _ = writeln!(s, "  group_by={}", stmt.group_by.len());
    }
    if !stmt.grouping_sets.is_empty() {
        let _ = writeln!(s, "  grouping_sets={}", stmt.grouping_sets.len());
    }
    if !stmt.order_by.is_empty() {
        let _ = writeln!(s, "  order_by={}", stmt.order_by.len());
    }
    if let Some(expr) = stmt.limit.as_ref() {
        let _ = writeln!(s, "  limit={}", explain_int_expr(expr));
    }
    if let Some(expr) = stmt.offset.as_ref() {
        let _ = writeln!(s, "  offset={}", explain_int_expr(expr));
    }
    if stmt.distinct {
        let _ = writeln!(s, "  distinct=true");
    }
    if !stmt.locking.is_empty() {
        let _ = writeln!(s, "  locking={}", stmt.locking.len());
    }
    s.trim_end().to_string()
}

pub(in crate::sql) fn should_defer_distinct_limit(stmt: &QueryBlockPlan) -> bool {
    stmt.distinct && (stmt.limit.is_some() || stmt.offset.is_some())
}

pub(in crate::sql) fn select_execution_stmt(
    stmt: &QueryBlockPlan,
    defer_distinct_limit: bool,
) -> QueryBlockPlan {
    if !defer_distinct_limit {
        return stmt.clone();
    }
    let mut exec_stmt = stmt.clone();
    exec_stmt.limit = None;
    exec_stmt.offset = None;
    exec_stmt
}

pub(in crate::sql) fn run_select_without_from_output(
    engine: &Engine,
    original: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let columns = projection_columns(&stmt.projections);
    let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::TableScan::from_physical_rows(
            uqa_execution::RowSchema::default(),
            vec![uqa_execution::PhysicalRow::from_values(Vec::new())],
        ));
    execute_query_block_operator_output(
        engine,
        operator,
        stmt.r#where.clone(),
        stmt,
        original,
        params,
        ctes,
        columns,
        output_mode,
    )
}
