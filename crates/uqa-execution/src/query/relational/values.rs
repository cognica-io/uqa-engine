//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Forward and directional VALUES physical execution.

use super::RelationalContext;
use crate::{
    query::{
        binding::values_types_in_scope,
        collection::collect_query_operator,
        consumer::{QueryConsumerControl, QueryOutputMode},
        output::{QueryOutput, QueryRows},
        CteScope,
    },
    scalar::plan::{eval_physical_scalar, PhysicalEvalContext},
    SharedExpressionEvaluator,
};
use uqa_sql::{
    plan::QueryPlan, type_resolution::coerce_common_context_value, ColumnType, SQLError, SQLParam,
    ScalarExpr,
};

pub fn execute_plan_values_output<S: Clone + 'static>(
    context: RelationalContext<'_, S>,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
    output_mode: QueryOutputMode<'_>,
) -> Result<QueryOutput, SQLError> {
    if rows.is_empty() {
        let scan: Box<dyn crate::PhysicalOperator + '_> = Box::new(
            crate::TableScan::from_physical_rows(crate::RowSchema::default(), Vec::new()),
        );
        return collect_query_operator(context.runtime, Vec::new(), scan, output_mode);
    }
    if matches!(
        &output_mode,
        QueryOutputMode::RowConsumer(consumer) if consumer.uses_directional_scan()
    ) {
        return execute_directional_values_output(
            context,
            rows,
            subqueries,
            params,
            ctes,
            output_mode,
        );
    }
    let columns: Vec<String> = (0..rows[0].len())
        .map(|index| format!("column{}", index + 1))
        .collect();
    let column_types =
        values_types_in_scope(context.catalog, rows, subqueries, None, params, ctes)?;
    let empty_schema = crate::RowSchema::default();
    let hook = context.expression_scope(ctes.clone());
    let evaluation = PhysicalEvalContext::new(None, params)
        .with_function_hook(hook.as_ref())
        .with_subquery_runner(hook.as_ref());
    let schema = crate::RowSchema::with_types(columns.clone(), column_types.clone());
    let consumer = match &output_mode {
        QueryOutputMode::RowConsumer(consumer) => {
            consumer.begin(&columns, &schema)?;
            Some(consumer)
        }
        QueryOutputMode::Rows | QueryOutputMode::SharedSpill | QueryOutputMode::ExistsKeySet => {
            None
        }
    };
    let mut output = consumer.is_none().then(|| Vec::with_capacity(rows.len()));
    for source in rows {
        if source.len() != columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "VALUES row width {} does not match first row width {}",
                source.len(),
                columns.len()
            )));
        }
        let mut values = Vec::with_capacity(source.len());
        for (index, expression) in source.iter().enumerate() {
            let source_type = crate::common_context_expression_type(
                expression,
                &empty_schema,
                params,
                Some(context.catalog),
            )?;
            let value = eval_physical_scalar(expression, subqueries, &evaluation)?;
            values.push(coerce_common_context_value(
                value,
                source_type.as_ref(),
                column_types[index].as_ref(),
            )?);
        }
        let row = crate::PhysicalRow::from_values(values);
        if let Some(consumer) = consumer {
            if matches!(
                consumer.consume(crate::OwnedPhysicalRow::new(schema.clone(), row),)?,
                QueryConsumerControl::Stop
            ) {
                break;
            }
        } else if let Some(output) = output.as_mut() {
            output.push(row);
        }
    }
    if consumer.is_some() {
        return Ok(QueryOutput {
            columns,
            column_types,
            internal_columns: schema.columns().to_vec(),
            internal_types: schema.column_types().to_vec(),
            rows: QueryRows::Rows {
                named: Vec::new(),
                positional: None,
            },
        });
    }
    let scan: Box<dyn crate::PhysicalOperator + '_> = Box::new(
        crate::TableScan::from_physical_rows(schema, output.unwrap_or_default()),
    );
    collect_query_operator(context.runtime, columns, scan, output_mode)
}

fn execute_directional_values_output<S: Clone + 'static>(
    context: RelationalContext<'_, S>,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
    output_mode: QueryOutputMode<'_>,
) -> Result<QueryOutput, SQLError> {
    let columns: Vec<String> = (0..rows[0].len())
        .map(|index| format!("column{}", index + 1))
        .collect();
    let column_types =
        values_types_in_scope(context.catalog, rows, subqueries, None, params, ctes)?;
    let empty_schema = crate::RowSchema::default();
    let source_types = rows
        .iter()
        .map(|source| {
            if source.len() != columns.len() {
                return Err(SQLError::TypeMismatch(format!(
                    "VALUES row width {} does not match first row width {}",
                    source.len(),
                    columns.len()
                )));
            }
            source
                .iter()
                .map(|expression| {
                    crate::common_context_expression_type(
                        expression,
                        &empty_schema,
                        params,
                        Some(context.catalog),
                    )
                })
                .collect::<Result<Vec<_>, SQLError>>()
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut values_ctes = ctes.clone();
    values_ctes.scalar_subqueries = subqueries.to_vec();
    let operator: Box<dyn crate::PhysicalOperator + '_> = Box::new(DirectionalValuesScan::new(
        rows,
        source_types,
        crate::RowSchema::with_types(columns.clone(), column_types),
        context.evaluator(params, &values_ctes),
    ));
    collect_query_operator(context.runtime, columns, operator, output_mode)
}

#[derive(Clone, Copy)]
enum ValuesScanPosition {
    BeforeFirst,
    OnRow(usize),
    AfterLast,
}

struct DirectionalValuesScan<'a> {
    rows: &'a [Vec<ScalarExpr>],
    source_types: Vec<Vec<Option<ColumnType>>>,
    schema: crate::RowSchema,
    evaluator: SharedExpressionEvaluator<'a>,
    position: ValuesScanPosition,
}

impl<'a> DirectionalValuesScan<'a> {
    fn new(
        rows: &'a [Vec<ScalarExpr>],
        source_types: Vec<Vec<Option<ColumnType>>>,
        schema: crate::RowSchema,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        Self {
            rows,
            source_types,
            schema,
            evaluator,
            position: ValuesScanPosition::BeforeFirst,
        }
    }

    fn evaluate(&self, position: usize) -> crate::ExecResult<crate::Batch> {
        let empty_schema = crate::RowSchema::default();
        let empty_row = crate::PhysicalRow::default();
        let values = self.rows[position]
            .iter()
            .enumerate()
            .map(|(column, expression)| {
                let value =
                    self.evaluator
                        .evaluate_physical(expression, &empty_schema, &empty_row)?;
                coerce_common_context_value(
                    value,
                    self.source_types[position][column].as_ref(),
                    self.schema.column_type(column),
                )
                .map_err(crate::ExecError::SQL)
            })
            .collect::<crate::ExecResult<Vec<_>>>()?;
        Ok(crate::Batch::from_physical_rows(
            self.schema.clone(),
            vec![crate::PhysicalRow::from_values(values)],
        ))
    }

    fn next_in_direction(
        &mut self,
        direction: crate::PhysicalScanDirection,
    ) -> crate::ExecResult<Option<crate::Batch>> {
        let target = match (direction, self.position) {
            (crate::PhysicalScanDirection::Forward, ValuesScanPosition::BeforeFirst) => 0,
            (crate::PhysicalScanDirection::Forward, ValuesScanPosition::OnRow(position)) => {
                position.saturating_add(1)
            }
            (crate::PhysicalScanDirection::Forward, ValuesScanPosition::AfterLast)
            | (crate::PhysicalScanDirection::Backward, ValuesScanPosition::BeforeFirst) => {
                return Ok(None)
            }
            (crate::PhysicalScanDirection::Backward, ValuesScanPosition::OnRow(0)) => {
                self.position = ValuesScanPosition::BeforeFirst;
                return Ok(None);
            }
            (crate::PhysicalScanDirection::Backward, ValuesScanPosition::OnRow(position)) => {
                position - 1
            }
            (crate::PhysicalScanDirection::Backward, ValuesScanPosition::AfterLast) => {
                let Some(position) = self.rows.len().checked_sub(1) else {
                    self.position = ValuesScanPosition::BeforeFirst;
                    return Ok(None);
                };
                position
            }
        };
        if target >= self.rows.len() {
            self.position = ValuesScanPosition::AfterLast;
            return Ok(None);
        }
        self.position = ValuesScanPosition::OnRow(target);
        self.evaluate(target).map(Some)
    }
}

impl crate::PhysicalOperator for DirectionalValuesScan<'_> {
    fn row_schema(&self) -> &crate::RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        u64::try_from(self.rows.len()).ok()
    }

    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        crate::BackwardScanSupport::Native
    }

    fn open(&mut self) -> crate::ExecResult<()> {
        self.position = ValuesScanPosition::BeforeFirst;
        Ok(())
    }

    fn next(&mut self) -> crate::ExecResult<Option<crate::Batch>> {
        self.next_in_direction(crate::PhysicalScanDirection::Forward)
    }

    fn next_direction(
        &mut self,
        direction: crate::PhysicalScanDirection,
    ) -> crate::ExecResult<Option<crate::Batch>> {
        self.next_in_direction(direction)
    }

    fn rewind(&mut self) -> crate::ExecResult<()> {
        self.position = ValuesScanPosition::BeforeFirst;
        Ok(())
    }

    fn close(&mut self) -> crate::ExecResult<()> {
        self.position = ValuesScanPosition::AfterLast;
        Ok(())
    }
}
