//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `QueryExpressionContext` adapter selecting adaptive or sort aggregation per grouping set.

use super::{QueryBlockPlan, QueryExpressionContext, SQLError, SQLParam, SpillBuffer};
use crate::{AggregateExecutor, Batch, ExecResult, ProjectedRow, RowSchema};

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

pub struct PhysicalAggregateExecutor<'a> {
    context: std::sync::Arc<dyn QueryExpressionContext + 'a>,
    params: &'a [SQLParam],

    input_row_schema: RowSchema,
    output_schema: RowSchema,
    output_budget: usize,
    sets: Vec<AggregateSet>,
}

impl<'a> PhysicalAggregateExecutor<'a> {
    pub fn new(
        context: std::sync::Arc<dyn QueryExpressionContext + 'a>,
        statement: &QueryBlockPlan,
        params: &'a [SQLParam],

        input_schema: RowSchema,
        output_schema: RowSchema,
        work_mem_bytes: usize,
    ) -> Result<Self, SQLError> {
        let runtime = context;
        let context = runtime.as_ref();
        let statements = grouping_set_statements(statement);
        let set_budget = (work_mem_bytes / statements.len().max(1)).max(1);
        let sets = statements
            .into_iter()
            .map(|(statement, relaxed)| {
                if super::adaptive::supports_adaptive_grouping(context, &statement) {
                    return super::adaptive::AdaptiveAggregateSet::new(
                        context,
                        statement,
                        relaxed,
                        set_budget,
                        &input_schema,
                        params,
                    )
                    .map(|set| AggregateSet::Adaptive(Box::new(set)));
                }
                let phase_budget = (set_budget / 3).max(1);
                let optimistic =
                    if super::adaptive::supports_optimistic_grouping(context, &statement) {
                        Some(Box::new(
                            super::adaptive::AdaptiveAggregateSet::new_optimistic(
                                context,
                                statement.clone(),
                                relaxed,
                                (set_budget / 2).max(1),
                                phase_budget,
                                &input_schema,
                                params,
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
            context: runtime,
            params,

            input_row_schema: input_schema,
            output_schema,
            output_budget: (work_mem_bytes / 3).max(1),
            sets,
        })
    }

    fn finish_set(&self, set: AggregateSet) -> Result<SpillBuffer, SQLError> {
        match set {
            AggregateSet::Adaptive(set) => {
                (*set).finish(self.context.as_ref(), &self.output_schema, self.params)
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
                        self.context.as_ref(),
                        &self.output_schema,
                        self.params,
                    );
                }
                super::sort_fallback::aggregate_sorted_input(
                    self.context.as_ref(),
                    &statement,
                    input,
                    &self.input_row_schema,
                    &self.output_schema,
                    self.params,
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
                    set.consume(self.context.as_ref(), &batch, self.params)?;
                }
                AggregateSet::Sorted {
                    input, optimistic, ..
                } => {
                    input.push(batch.clone())?;
                    if let Some(candidate) = optimistic.as_mut() {
                        candidate.consume(self.context.as_ref(), &batch, self.params)?;
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

    fn supports_storage_borrowed_rows(&self) -> bool {
        self.sets.iter().all(|set| {
            matches!(set, AggregateSet::Adaptive(set) if set.supports_storage_borrowed_rows())
        })
    }

    fn consume_projected_row(&mut self, row: &ProjectedRow<'_, '_>) -> ExecResult<()> {
        for set in &mut self.sets {
            let AggregateSet::Adaptive(set) = set else {
                return Err(crate::ExecError::Other(
                    "sort aggregate cannot consume a projected row".into(),
                ));
            };
            set.consume_projected_row(self.context.as_ref(), row, self.params)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> ExecResult<SpillBuffer> {
        let mut sets = std::mem::take(&mut self.sets);
        if sets.len() == 1 {
            let set = sets.pop().ok_or_else(|| {
                crate::ExecError::Other("aggregate grouping set disappeared".into())
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
                    crate::ExecError::Other("aggregate output row count overflow".into())
                })?;
            copy_output(&mut set_output, &mut output)?;
        }
        if output.rows() != expected_output_rows {
            return Err(crate::ExecError::Other(format!(
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
            statement.with_ties = false;
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
        copied = copied
            .checked_add(batch.rows.len())
            .ok_or_else(|| crate::ExecError::Other("aggregate copied row count overflow".into()))?;
        destination.push(batch)?;
    }
    if copied != expected {
        return Err(crate::ExecError::Other(format!(
            "aggregate spill drain returned {copied} rows, expected {expected}"
        )));
    }
    Ok(())
}
