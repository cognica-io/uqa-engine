//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine adapter selecting adaptive or sort aggregation per grouping set.

use super::{
    projection_columns, CteScope, Engine, QueryBlockPlan, SQLError, SQLParam, SpillBuffer,
};
use uqa_execution::{AggregateExecutor, Batch, ExecResult, RowSchema};
use uqa_sql::expr::RowLookup;

enum AggregateSet {
    Adaptive(Box<super::adaptive::AdaptiveAggregateSet>),
    Sorted {
        statement: Box<QueryBlockPlan>,
        relaxed: bool,
        input: SpillBuffer,
        phase_budget: usize,
        optimistic: Option<Box<super::adaptive::AdaptiveAggregateSet>>,
    },
}

pub(in crate::sql) struct PhysicalAggregateExecutor<'a> {
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    input_schema: Vec<String>,
    output_schema: RowSchema,
    output_budget: usize,
    sets: Vec<AggregateSet>,
}

impl<'a> PhysicalAggregateExecutor<'a> {
    pub(in crate::sql) fn new(
        engine: &'a Engine,
        statement: &QueryBlockPlan,
        params: &'a [SQLParam],
        ctes: &'a CteScope,
        input_schema: Vec<String>,
        work_mem_bytes: usize,
    ) -> Result<Self, SQLError> {
        let statements = grouping_set_statements(statement);
        let set_budget = (work_mem_bytes / statements.len().max(1)).max(1);
        let sets = statements
            .into_iter()
            .map(|(statement, relaxed)| {
                if super::adaptive::supports_adaptive_grouping(engine, &statement) {
                    return super::adaptive::AdaptiveAggregateSet::new(
                        engine,
                        statement,
                        relaxed,
                        set_budget,
                        &input_schema,
                    )
                    .map(|set| AggregateSet::Adaptive(Box::new(set)));
                }
                let phase_budget = (set_budget / 3).max(1);
                let optimistic =
                    if super::adaptive::supports_optimistic_grouping(engine, &statement) {
                        Some(Box::new(
                            super::adaptive::AdaptiveAggregateSet::new_optimistic(
                                engine,
                                statement.clone(),
                                relaxed,
                                (set_budget / 2).max(1),
                                phase_budget,
                                &input_schema,
                            )?,
                        ))
                    } else {
                        None
                    };
                let input_budget = if optimistic.is_some() {
                    (set_budget / 2).max(1)
                } else {
                    phase_budget
                };
                Ok(AggregateSet::Sorted {
                    statement: Box::new(statement),
                    relaxed,
                    input: SpillBuffer::new(input_budget),
                    phase_budget,
                    optimistic,
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        Ok(Self {
            engine,
            params,
            ctes,
            input_schema,
            output_schema: RowSchema::new(projection_columns(&statement.projections)),
            output_budget: (work_mem_bytes / 3).max(1),
            sets,
        })
    }

    fn finish_set(&self, set: AggregateSet) -> Result<SpillBuffer, SQLError> {
        match set {
            AggregateSet::Adaptive(set) => {
                (*set).finish(self.engine, &self.output_schema, self.params, self.ctes)
            }
            AggregateSet::Sorted {
                statement,
                relaxed,
                input,
                phase_budget,
                optimistic,
            } => {
                if let Some(optimistic) = optimistic {
                    debug_assert!(!optimistic.is_abandoned());
                    drop(input);
                    return (*optimistic).finish(
                        self.engine,
                        &self.output_schema,
                        self.params,
                        self.ctes,
                    );
                }
                super::sort_fallback::aggregate_sorted_input(
                    self.engine,
                    &statement,
                    input,
                    &self.input_schema,
                    &self.output_schema,
                    self.params,
                    self.ctes,
                    phase_budget,
                    relaxed,
                )
            }
        }
    }
}

impl AggregateExecutor for PhysicalAggregateExecutor<'_> {
    fn consume(&mut self, batch: Batch) -> ExecResult<()> {
        for set in &mut self.sets {
            match set {
                AggregateSet::Adaptive(set) => {
                    set.consume(self.engine, &batch, self.params, self.ctes)?;
                }
                AggregateSet::Sorted {
                    input, optimistic, ..
                } => {
                    input.push(batch.clone())?;
                    if let Some(candidate) = optimistic.as_mut() {
                        candidate.consume(self.engine, &batch, self.params, self.ctes)?;
                        if candidate.is_abandoned() {
                            *optimistic = None;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn supports_projected_rows(&self) -> bool {
        self.sets.iter().all(|set| {
            matches!(set, AggregateSet::Adaptive(set) if set.statement_subqueries_are_empty())
        })
    }

    fn consume_projected_row(&mut self, row: &dyn RowLookup) -> ExecResult<()> {
        for set in &mut self.sets {
            let AggregateSet::Adaptive(set) = set else {
                return Err(uqa_execution::ExecError::Other(
                    "sort aggregate cannot consume a projected row".into(),
                ));
            };
            set.consume_projected_row(self.engine, row, self.params, self.ctes)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> ExecResult<SpillBuffer> {
        let mut sets = std::mem::take(&mut self.sets);
        if sets.len() == 1 {
            let set = sets.pop().ok_or_else(|| {
                uqa_execution::ExecError::Other("aggregate grouping set disappeared".into())
            })?;
            return self.finish_set(set).map_err(Into::into);
        }

        let mut output = SpillBuffer::new(self.output_budget);
        let mut expected_output_rows = 0usize;
        for set in sets {
            let mut set_output = self.finish_set(set)?;
            expected_output_rows = expected_output_rows
                .checked_add(set_output.rows())
                .ok_or_else(|| {
                    uqa_execution::ExecError::Other("aggregate output row count overflow".into())
                })?;
            copy_output(&mut set_output, &mut output)?;
        }
        if output.rows() != expected_output_rows {
            return Err(uqa_execution::ExecError::Other(format!(
                "aggregate output retained {} rows, expected {expected_output_rows}",
                output.rows()
            )));
        }
        Ok(output)
    }
}

fn grouping_set_statements(statement: &QueryBlockPlan) -> Vec<(QueryBlockPlan, bool)> {
    let sets = if statement.grouping_sets.is_empty() {
        vec![(statement.clone(), false)]
    } else {
        statement
            .grouping_sets
            .iter()
            .map(|group_by| {
                let mut active = statement.clone();
                active.group_by.clone_from(group_by);
                active.grouping_sets.clear();
                (active, true)
            })
            .collect()
    };
    sets.into_iter()
        .map(|(mut statement, relaxed)| {
            statement.order_by.clear();
            statement.limit = None;
            statement.offset = None;
            (statement, relaxed)
        })
        .collect()
}

fn copy_output(source: &mut SpillBuffer, destination: &mut SpillBuffer) -> ExecResult<()> {
    let expected = source.rows();
    let mut copied = 0usize;
    for batch in source.drain()? {
        let batch = batch?;
        copied = copied.checked_add(batch.rows.len()).ok_or_else(|| {
            uqa_execution::ExecError::Other("aggregate copied row count overflow".into())
        })?;
        destination.push(batch)?;
    }
    if copied != expected {
        return Err(uqa_execution::ExecError::Other(format!(
            "aggregate spill drain returned {copied} rows, expected {expected}"
        )));
    }
    Ok(())
}
