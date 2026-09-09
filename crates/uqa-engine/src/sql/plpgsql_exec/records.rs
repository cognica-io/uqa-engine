//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime record descriptors and positional trigger-result validation.

use super::{
    ColumnType, Expr, Interpreter, PLpgSQLDatum, PLpgSQLReturnValue, RoutineOutcome, SQLError,
    TriggerRoutineContext, Value,
};

impl Interpreter<'_> {
    pub(super) fn expression_type(&self, expr: &Expr) -> Result<Option<ColumnType>, SQLError> {
        let bound = super::bind_expr(expr, &mut self.resolver())?;
        let plan = uqa_planner::ExpressionPlan::lower(bound);
        let mut scope = crate::sql::CteScope::new_for_current_routine(self.engine);
        scope.scalar_subqueries.clone_from(&plan.subqueries);
        let hook = crate::sql::ScopedEngineHook::new(self.engine, &scope);
        uqa_execution::scalar_type_with_resolver(
            &plan.scalar,
            &uqa_execution::RowSchema::default(),
            &[],
            &hook,
        )
    }

    pub(super) fn datum_type(&self, index: usize) -> Option<ColumnType> {
        self.resolver().datum_type(index)
    }

    pub(super) fn record_expression_types(
        &self,
        expr: &Expr,
    ) -> Result<Option<Vec<Option<ColumnType>>>, SQLError> {
        match expr {
            Expr::Column(name) | Expr::QualifiedStar(name) => Ok(self
                .resolver()
                .lookup(name)
                .and_then(|index| self.record_types.get(&index).cloned())),
            Expr::Row(fields) => fields
                .iter()
                .map(|field| self.expression_type(field))
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Expr::Cast { expr, ty } if ty == "record" => self.record_expression_types(expr),
            _ => Ok(None),
        }
    }

    pub(super) fn return_record_types(
        &self,
        value: &PLpgSQLReturnValue,
    ) -> Result<Option<Vec<Option<ColumnType>>>, SQLError> {
        match value {
            PLpgSQLReturnValue::Expr(expr) => self.record_expression_types(expr),
            PLpgSQLReturnValue::Datum(index) => match &self.datums[*index] {
                PLpgSQLDatum::Row { fields } => Ok(Some(
                    fields
                        .iter()
                        .map(|field| self.datum_type(field.varno))
                        .collect(),
                )),
                _ => Ok(self.record_types.get(index).cloned()),
            },
        }
    }

    pub(super) fn return_value_type(
        &self,
        value: &PLpgSQLReturnValue,
    ) -> Result<Option<ColumnType>, SQLError> {
        match value {
            PLpgSQLReturnValue::Expr(expr) => self.expression_type(expr),
            PLpgSQLReturnValue::Datum(index) => Ok(self.datum_type(*index)),
        }
    }
}

pub(super) fn shape_trigger_outcome(
    outcome: RoutineOutcome,
    context: &TriggerRoutineContext,
) -> Result<Value, SQLError> {
    let values = match outcome.value {
        Value::Null => return Ok(Value::Null),
        Value::Record(fields) => fields
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>(),
        Value::Row(values) => values,
        _ => return Err(trigger_shape_error()),
    };
    let types = outcome.anonymous_record_column_types.unwrap_or_else(|| {
        values
            .iter()
            .map(super::handlers::runtime_record_column_type)
            .collect()
    });
    if values.len() != context.column_types.len()
        || types.len() != context.column_types.len()
        || types
            .iter()
            .zip(&context.column_types)
            .any(|(source, target)| !record_types_match(source.as_ref(), target.as_ref()))
    {
        return Err(trigger_shape_error());
    }
    let fields = match (&context.new, &context.old) {
        (Value::Record(fields), _) | (_, Value::Record(fields)) => fields,
        _ => return Err(trigger_shape_error()),
    };
    Ok(Value::Record(
        fields
            .iter()
            .map(|(name, _)| name.clone())
            .zip(values)
            .collect(),
    ))
}

fn record_types_match(source: Option<&ColumnType>, target: Option<&ColumnType>) -> bool {
    match (source, target) {
        (
            Some(ColumnType::Domain { oid: source, .. }),
            Some(ColumnType::Domain { oid: target, .. }),
        ) => source == target,
        (Some(ColumnType::Domain { .. }), _) | (_, Some(ColumnType::Domain { .. })) => false,
        (Some(source), Some(target)) => {
            let source = crate::sql::postgres_result_type(source);
            let target = crate::sql::postgres_result_type(target);
            source.type_oid == target.type_oid
                && (target.type_modifier < 0 || source.type_modifier == target.type_modifier)
        }
        _ => false,
    }
}

fn trigger_shape_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42804".into(),
        message: "returned row structure does not match the structure of the triggering table"
            .into(),
    }
}
