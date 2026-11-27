//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive UNION execution with demand-driven work tables and shared spill-backed duplicate tracking.

use super::{materialize_plan_ctes, CteExecutionContext};
use crate::eval_scalar;
use crate::query::{
    binding::analyze_recursive_control_step,
    collection::collect_query_operator,
    consumer::QueryOutputMode,
    output::{QueryOutput, QueryRows},
    projection::{physical_exec_error, physical_work_mem_bytes},
    CteScope,
};
use uqa_core::ArrayValue;
use uqa_core::Value;
use uqa_sql::{
    ast::SetOpKind,
    plan::{CtePlan, ProjectionPlan, RelationalPlan, SourcePlan},
    semantics::query_plan_output_columns_with_control,
    SQLError, SQLParam, ScalarExpr,
};

mod keys;

/// Drive the recursive producer to completion for callers that require an immutable result.
pub fn materialize_recursive_cte<S: Clone>(
    context: CteExecutionContext<'_, S>,
    cte: &CtePlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_filter: Option<&uqa_sql::plan::source_projection::CteOutputFilter>,
) -> Result<crate::SharedSpill, SQLError> {
    let (input, _) = build_recursive_cte(context, cte, params, ctes, output_filter)?;
    collect_shared(context, input.schema().to_vec(), input)
}

/// Iterate the recursive `CtePlan`: take the anchor (LHS of UNION ALL) as
/// the initial row set, then repeatedly evaluate the recursive step
/// (RHS) with the `CtePlan` bound to the *new rows from the previous
/// iteration* (working set), unioning the result back into the total.
/// Each working-set transition observes the active statement cancellation and deadline.
#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(super) fn build_recursive_cte<S: Clone>(
    context: CteExecutionContext<'_, S>,
    cte: &CtePlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_filter: Option<&uqa_sql::plan::source_projection::CteOutputFilter>,
) -> Result<(Box<dyn crate::PhysicalOperator>, usize), SQLError> {
    let query = cte.body.query().ok_or_else(|| SQLError::Routine {
        sqlstate: "42P19".into(),
        message: format!(
            "recursive query \"{}\" must not contain data-modifying statements",
            cte.name
        ),
    })?;
    if !query.ctes.is_empty() {
        materialize_plan_ctes(context, &query.ctes, params, ctes)?;
    }

    let RelationalPlan::SetOp {
        kind,
        all,
        left,
        right,
        order_by,
        limit,
        offset,
        ..
    } = &query.root
    else {
        return Err(SQLError::Unsupported(
            "recursive CTE requires a UNION query".into(),
        ));
    };
    if *kind != SetOpKind::Union {
        return Err(SQLError::Unsupported(
            "recursive CTE only supports UNION".into(),
        ));
    }
    reject_recursive_set_ordering(order_by, limit.as_deref(), offset.as_deref())?;

    let declared_columns = (!cte.columns.is_empty()).then_some(cte.columns.as_slice());
    let controls_recursion = cte.search.is_some() || cte.cycle.is_some();
    let specialized = if controls_recursion {
        None
    } else if let Some((qualifier, filter)) = output_filter {
        let resources = context.production_resources()?;
        let inferred = if declared_columns.is_none() {
            query_plan_output_columns_with_control(left, &resources.control())?
        } else {
            None
        };
        let output_columns = declared_columns.or_else(|| inferred.as_deref().map(Vec::as_slice));
        match output_columns {
            Some(output_columns) => {
                let specialized_anchor = context.rewrites.push_output_filter(
                    left,
                    qualifier,
                    filter,
                    Some(output_columns),
                )?;
                let specialized_step = context.rewrites.push_output_filter(
                    right,
                    qualifier,
                    filter,
                    Some(output_columns),
                )?;
                match (specialized_anchor, specialized_step) {
                    (Some(anchor), Some(step)) => Some((anchor, step)),
                    _ => None,
                }
            }
            None => None,
        }
    } else {
        None
    };
    let (anchor_plan, step_plan) = specialized
        .as_ref()
        .map_or((left.as_ref(), right.as_ref()), |(anchor, step)| {
            (anchor.as_ref(), step.as_ref())
        });

    let resources = context.production_resources()?;
    let control = resources.control();
    let seed = context
        .queries
        .retain_recursive_term(anchor_plan, params, ctes)?
        .scan(None)?;
    let seed = compact_term(seed, &cte.columns);
    let base = seed.row_schema().clone();
    let mut schema = base.clone();
    let mut annotations = None;
    let controlled;
    let plan = if controls_recursion {
        analyze_recursive_control_step(
            context.routines,
            cte,
            step_plan,
            base.clone(),
            params,
            ctes,
        )?;
        schema = crate::query::binding::extend_cte_generated_schema(
            context.routines,
            cte,
            base.clone(),
            params,
        )?;
        controlled = controlled_recursive_step_plan(cte, step_plan)?;
        let search_positions = cte
            .search
            .as_ref()
            .map(|s| resolve_control_positions(&base, &s.columns, "search"))
            .transpose()?;
        let cycle_positions = cte
            .cycle
            .as_ref()
            .map(|s| resolve_control_positions(&base, &s.columns, "cycle"))
            .transpose()?;
        let cycle_marks = cte
            .cycle
            .as_ref()
            .map(|cycle| cycle_mark_values(context, cycle, params, &schema))
            .transpose()?;
        annotations = Some(Annotations {
            breadth_first: cte.search.as_ref().is_some_and(|s| s.breadth_first),
            search_positions,
            cycle_positions,
            cycle_marks,
            parent_search: controlled.parent_search,
            parent_cycle: controlled.parent_cycle_path,
        });
        &controlled.plan
    } else {
        step_plan
    };
    let step = context.queries.retain_recursive_term(plan, params, ctes)?;
    let budget = physical_work_mem_bytes(context.runtime)?.max(1);
    // The shared prefix, two successive work tables and, for UNION, the seen set can be live together.
    let state_budget = (budget / if *all { 3 } else { 4 }).max(1);
    let operator = RecursiveUnion {
        name: control.copy_text(&cte.name)?,
        base,
        schema,
        annotations,
        step,
        current: Some(seed),
        work: crate::SpillBuffer::new(state_budget),
        work_budget: state_budget,
        seen: (!*all).then(|| crate::ExactRowSet::new(state_budget)),
        depth: 0,
        resources: resources.clone(),
        _memory: control.reserve(size_of::<RecursiveUnion>())?,
    };
    Ok((Box::new(operator), state_budget))
}

fn compact_term<'a>(
    input: Box<dyn crate::PhysicalOperator + 'a>,
    aliases: &[String],
) -> Box<dyn crate::PhysicalOperator + 'a> {
    let columns = input
        .schema()
        .iter()
        .enumerate()
        .map(|(index, name)| (aliases.get(index).unwrap_or(name).clone(), index))
        .collect();
    Box::new(crate::ColumnSelection::compacting_with_positions(
        input, columns,
    ))
}

fn reject_recursive_set_ordering(
    order_by: &[uqa_sql::plan::OrderPlan],
    limit: Option<&ScalarExpr>,
    offset: Option<&ScalarExpr>,
) -> Result<(), SQLError> {
    if !order_by.is_empty() {
        return Err(SQLError::Unsupported(
            "ORDER BY in a recursive query is not implemented".into(),
        ));
    }
    if offset.is_some() {
        return Err(SQLError::Unsupported(
            "OFFSET in a recursive query is not implemented".into(),
        ));
    }
    if limit.is_some() {
        return Err(SQLError::Unsupported(
            "LIMIT in a recursive query is not implemented".into(),
        ));
    }
    Ok(())
}

struct ControlledRecursiveStep {
    plan: uqa_sql::plan::QueryPlan,
    parent_search: bool,
    parent_cycle_path: bool,
}

fn controlled_recursive_step_plan(
    cte: &CtePlan,
    step: &uqa_sql::plan::QueryPlan,
) -> Result<ControlledRecursiveStep, SQLError> {
    let mut plan = step.clone();
    let parent_search = cte
        .search
        .as_ref()
        .is_some_and(|search| !search.breadth_first);
    let parent_cycle_path = cte.cycle.is_some();
    if !parent_search && !parent_cycle_path {
        return Ok(ControlledRecursiveStep {
            plan,
            parent_search,
            parent_cycle_path,
        });
    }
    let RelationalPlan::QueryBlock(block) = &mut plan.root else {
        return Err(SQLError::Unsupported(
            "recursive SEARCH/CYCLE term must be one SELECT query block".into(),
        ));
    };
    let qualifier = block
        .from
        .as_ref()
        .and_then(|source| recursive_reference_qualifier(source, &cte.name))
        .ok_or_else(|| {
            SQLError::Unsupported(
                "recursive SEARCH/CYCLE reference must be visible in the recursive term".into(),
            )
        })?;
    if parent_search {
        let search = cte.search.as_ref().expect("depth-first SEARCH");
        block.projections.push(ProjectionPlan {
            expr: ScalarExpr::qualified_column(&qualifier, &search.sequence_column),
            alias: None,
        });
    }
    if parent_cycle_path {
        let cycle = cte.cycle.as_ref().expect("CYCLE clause");
        block.projections.push(ProjectionPlan {
            expr: ScalarExpr::qualified_column(qualifier, &cycle.path_column),
            alias: None,
        });
    }
    Ok(ControlledRecursiveStep {
        plan,
        parent_search,
        parent_cycle_path,
    })
}

fn recursive_reference_qualifier(source: &SourcePlan, cte_name: &str) -> Option<String> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } if uqa_sql::semantics::cte_reference_name(name).as_deref() == Some(cte_name) => {
            Some(alias.as_deref().unwrap_or(qualifier).to_string())
        }
        SourcePlan::Join { left, right, .. } => recursive_reference_qualifier(left, cte_name)
            .or_else(|| recursive_reference_qualifier(right, cte_name)),
        SourcePlan::RetainedRows(_)
        | SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => None,
    }
}

fn resolve_control_positions(
    schema: &crate::RowSchema,
    columns: &[String],
    kind: &str,
) -> Result<Vec<usize>, SQLError> {
    columns
        .iter()
        .map(|column| {
            schema
                .columns()
                .iter()
                .position(|candidate| candidate == column)
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: format!("{kind} column \"{column}\" not in WITH query column list"),
                })
        })
        .collect()
}

fn cycle_mark_values<S: Clone>(
    context: CteExecutionContext<'_, S>,
    cycle: &uqa_sql::plan::CteCyclePlan,
    params: &[SQLParam],
    schema: &crate::RowSchema,
) -> Result<(Value, Value), SQLError> {
    let empty = crate::RowSchema::default();
    let mark_type = schema
        .columns()
        .iter()
        .position(|column| column == &cycle.mark_column)
        .and_then(|position| schema.column_type(position));
    let value_type =
        crate::scalar_type_with_resolver(&cycle.mark_value, &empty, params, context.routines)?;
    let default_type =
        crate::scalar_type_with_resolver(&cycle.mark_default, &empty, params, context.routines)?;
    let context = crate::ScalarEvalContext::new(None, params).with_function_hook(context.functions);
    let mark = uqa_sql::type_resolution::coerce_common_context_value(
        eval_scalar(&cycle.mark_value, &context)?,
        value_type.as_ref(),
        mark_type,
    )?;
    let default = uqa_sql::type_resolution::coerce_common_context_value(
        eval_scalar(&cycle.mark_default, &context)?,
        default_type.as_ref(),
        mark_type,
    )?;
    Ok((mark, default))
}

#[derive(Default)]
struct RecursiveParentState {
    search_sequence: Option<Value>,
    cycle_path: Option<Value>,
}

struct Annotations {
    breadth_first: bool,
    search_positions: Option<Vec<usize>>,
    cycle_positions: Option<Vec<usize>>,
    cycle_marks: Option<(Value, Value)>,
    parent_search: bool,
    parent_cycle: bool,
}

/// A generation reads one term. Accepted rows are returned immediately and enter the next work table once. A recursive term is constructed only after a reader crosses the end of the preceding generation.
struct RecursiveUnion {
    name: uqa_core::memory::Produced<String>,
    base: crate::RowSchema,
    schema: crate::RowSchema,
    annotations: Option<Annotations>,
    step: Box<dyn super::context::RecursiveTerm>,
    current: Option<Box<dyn crate::PhysicalOperator>>,
    work: crate::SpillBuffer,
    work_budget: usize,
    seen: Option<crate::ExactRowSet>,
    depth: usize,
    resources: uqa_core::memory::OwnedProductionControl,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

impl RecursiveUnion {
    fn accept(&mut self, batch: crate::Batch) -> crate::ExecResult<Option<crate::Batch>> {
        let catalog = uqa_sql::expr::EngineValueCatalog(self.step.functions());
        let mut emitted = Vec::with_capacity(batch.rows.len());
        let mut expandable = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            self.resources.control().check().map_err(SQLError::from)?;
            let (row, cycle) = if let Some(annotations) = &self.annotations {
                annotations.annotate(&batch.schema, row, &self.base, self.depth, &catalog)?
            } else {
                (row, false)
            };
            if let Some(seen) = &mut self.seen {
                if !seen.insert_sql_physical(&row, &self.schema, Some(&catalog))? {
                    continue;
                }
            }
            if !cycle {
                expandable.push(row.clone());
            }
            emitted.push(row);
        }
        if !expandable.is_empty() {
            self.work.push(crate::Batch::from_physical_rows(
                self.schema.clone(),
                expandable,
            ))?;
        }
        Ok((!emitted.is_empty())
            .then(|| crate::Batch::from_physical_rows(self.schema.clone(), emitted)))
    }
}

impl crate::PhysicalOperator for RecursiveUnion {
    fn row_schema(&self) -> &crate::RowSchema {
        &self.schema
    }
    fn open(&mut self) -> crate::ExecResult<()> {
        if let Some(current) = &mut self.current {
            current.open()?;
        }
        Ok(())
    }
    fn next(&mut self) -> crate::ExecResult<Option<crate::Batch>> {
        self.next_with_demand(
            std::num::NonZeroUsize::new(crate::DEFAULT_BATCH_SIZE).expect("nonzero batch size"),
        )
    }
    fn next_with_demand(
        &mut self,
        demand: std::num::NonZeroUsize,
    ) -> crate::ExecResult<Option<crate::Batch>> {
        while let Some(current) = &mut self.current {
            self.resources.control().check().map_err(SQLError::from)?;
            if let Some(batch) = current.next_with_demand(demand)? {
                if let Some(output) = self.accept(batch)? {
                    return Ok(Some(output));
                }
                continue;
            }
            self.close()?;
            if self.work.rows() == 0 {
                break;
            }
            let previous =
                std::mem::replace(&mut self.work, crate::SpillBuffer::new(self.work_budget))
                    .into_shared(self.schema.clone())?;
            self.depth = self.depth.checked_add(1).ok_or_else(|| {
                SQLError::Unsupported("recursive CTE depth exceeds bigint".into())
            })?;
            let width = self.annotations.as_ref().map(|_| self.base.len());
            let term = self.step.scan(Some((&self.name, previous, width)))?;
            self.current = Some(compact_term(term, self.base.columns()));
            self.open()?;
        }
        Ok(None)
    }
    fn close(&mut self) -> crate::ExecResult<()> {
        if let Some(mut current) = self.current.take() {
            current.close()?;
        }
        Ok(())
    }
}

impl Annotations {
    fn annotate(
        &self,
        input: &crate::RowSchema,
        row: crate::PhysicalRow,
        base: &crate::RowSchema,
        depth: usize,
        catalog: &dyn uqa_sql::expr::SQLValueCatalog,
    ) -> Result<(crate::PhysicalRow, bool), SQLError> {
        let has_parent = depth != 0;
        let with_search = has_parent && self.parent_search;
        let with_cycle = has_parent && self.parent_cycle;
        let expected = base.len() + usize::from(with_search) + usize::from(with_cycle);
        if input.len() != expected {
            return Err(SQLError::TypeMismatch(format!(
                "recursive term has {} columns but expected {expected}",
                input.len()
            )));
        }
        let source = crate::OwnedPhysicalRow::new(input.clone(), row);
        let view = source.view();
        let value = |index| {
            view.value_at(index)
                .cloned()
                .ok_or_else(|| SQLError::Internal(format!("recursive term lost attribute {index}")))
        };
        let parent = RecursiveParentState {
            search_sequence: with_search.then(|| value(base.len())).transpose()?,
            cycle_path: with_cycle
                .then(|| value(base.len() + usize::from(with_search)))
                .transpose()?,
        };
        let base = crate::OwnedPhysicalRow::new(
            base.clone(),
            crate::PhysicalRow::from_values((0..base.len()).map(value).collect::<Result<_, _>>()?),
        );
        annotate_recursive_row(&base, self, &parent, depth, catalog)
    }
}

fn annotate_recursive_row(
    base: &crate::OwnedPhysicalRow,
    annotations: &Annotations,
    parent: &RecursiveParentState,
    depth: usize,
    catalog: &dyn uqa_sql::expr::SQLValueCatalog,
) -> Result<(crate::PhysicalRow, bool), SQLError> {
    let mut values = base
        .view()
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    if let Some(positions) = &annotations.search_positions {
        let sequence = if annotations.breadth_first {
            let depth = i64::try_from(depth)
                .map_err(|_| SQLError::Unsupported("recursive CTE depth exceeds bigint".into()))?;
            keys::record(base, positions, Some(depth))?
        } else {
            let mut path = parent
                .search_sequence
                .as_ref()
                .and_then(|value| match value {
                    Value::Array(array) => Some(array.elements().to_vec()),
                    _ => None,
                })
                .unwrap_or_default();
            path.push(keys::record(base, positions, None)?);
            Value::Array(ArrayValue::try_new(path).ok_or_else(|| {
                SQLError::Internal("SEARCH path is not a rectangular SQL array".into())
            })?)
        };
        values.push(sequence);
    }
    let mut is_cycle = false;
    if let (Some(positions), Some((mark, default))) =
        (&annotations.cycle_positions, &annotations.cycle_marks)
    {
        let key = keys::record(base, positions, None)?;
        let mut path = parent
            .cycle_path
            .as_ref()
            .and_then(|value| match value {
                Value::Array(array) => Some(array.elements().to_vec()),
                _ => None,
            })
            .unwrap_or_default();
        for existing in &path {
            if uqa_sql::expr::equal_typed_values_with_enum_catalog(
                existing,
                &key,
                &uqa_core::memory::ProductionControl::uncontrolled(),
                Some(catalog),
            )? {
                is_cycle = true;
                break;
            }
        }
        path.push(key);
        values.push(if is_cycle {
            mark.clone()
        } else {
            default.clone()
        });
        values.push(Value::Array(ArrayValue::try_new(path).ok_or_else(
            || SQLError::Internal("CYCLE path is not a rectangular SQL array".into()),
        )?));
    }
    Ok((crate::PhysicalRow::from_values(values), is_cycle))
}

pub fn alias_query_output_to_shared<S: Clone>(
    context: CteExecutionContext<'_, S>,
    output: QueryOutput,
    aliases: &[String],
) -> Result<crate::SharedSpill, SQLError> {
    let visible_source_columns = output.columns.clone();
    let source_columns = output.internal_schema.columns().to_vec();
    let columns = visible_source_columns
        .iter()
        .enumerate()
        .map(|(index, source)| {
            aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| source.clone())
        })
        .collect::<Vec<_>>();
    let mut operator = output.into_operator();
    if source_columns != columns {
        let mapping = source_columns
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let output = columns
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| source.clone());
                (output, index)
            })
            .collect::<Vec<_>>();
        operator = Box::new(crate::ColumnSelection::with_positions(operator, mapping));
    }
    collect_shared(context, columns, compact_term(operator, &[]))
}

fn collect_shared<S: Clone>(
    context: CteExecutionContext<'_, S>,
    columns: Vec<String>,
    operator: Box<dyn crate::PhysicalOperator + '_>,
) -> Result<crate::SharedSpill, SQLError> {
    let output = collect_query_operator(
        context.runtime,
        columns,
        operator,
        QueryOutputMode::SharedSpill,
    )?;
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(
            "CTE collector returned in-memory rows".into(),
        ));
    };
    Ok(rows)
}

pub fn append_shared_spill(
    output: &mut crate::SpillBuffer,
    rows: &crate::SharedSpill,
) -> Result<(), SQLError> {
    let reader = rows.reader().map_err(physical_exec_error)?;
    for batch in reader {
        output
            .push(batch.map_err(physical_exec_error)?)
            .map_err(physical_exec_error)?;
    }
    Ok(())
}

pub fn filter_new_recursive_rows(
    input: &crate::SharedSpill,
    seen: &mut crate::ExactRowSet,
    catalog: &dyn uqa_sql::expr::SQLValueCatalog,
) -> Result<crate::SharedSpill, SQLError> {
    // The source is already disk-backed. Retain no cardinality-sized tail
    // while constructing the next working set.
    let mut output = crate::SpillBuffer::new(1);
    let schema = input.row_schema().clone();
    let reader = input.reader().map_err(physical_exec_error)?;
    for batch in reader {
        let batch = batch.map_err(physical_exec_error)?;
        let mut rows = Vec::with_capacity(batch.rows.len().min(crate::DEFAULT_BATCH_SIZE));
        for row in batch.rows {
            if seen
                .insert_sql_physical(&row, &batch.schema, Some(catalog))
                .map_err(physical_exec_error)?
            {
                rows.push(row);
            }
        }
        if !rows.is_empty() {
            output
                .push(crate::Batch::from_physical_rows(schema.clone(), rows))
                .map_err(physical_exec_error)?;
        }
    }
    output.into_shared(schema).map_err(physical_exec_error)
}
