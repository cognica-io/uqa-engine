//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime record descriptors and positional trigger-result validation.

use super::{
    ColumnType, Expr, Interpreter, PLpgSQLDatum, PLpgSQLExpression, PLpgSQLReturnValue,
    RoutineOutcome, SQLError, TriggerRoutineContext, Value,
};

impl<'a> Interpreter<'a> {
    pub(super) fn set_record_target(
        &mut self,
        catalog: &'a dyn uqa_sql::routines::declaration::RoutineTypeCatalog,
        target: Option<&[uqa_sql::routines::result_check::SQLFunctionResultColumn]>,
    ) {
        self.record_target = target.map(|columns| super::RecordReturnTarget {
            catalog,
            columns: columns.to_vec(),
        });
    }

    pub(super) fn validate_record_return(
        &self,
        value: &Value,
        declared: Option<&[Option<ColumnType>]>,
    ) -> Result<(), SQLError> {
        let Some(target) = &self.record_target else {
            return Ok(());
        };
        if matches!(value, Value::Null) {
            return Ok(());
        }
        if let Value::Row(row) = value {
            if let Some(fields) = row.field_types() {
                return uqa_sql::routines::result_check::validate_procedural_record_identity(
                    target.catalog,
                    fields,
                    &target.columns,
                );
            }
        }
        let inferred;
        let source = if let Some(declared) = declared {
            declared
        } else {
            inferred = match value {
                Value::Row(row) => row
                    .iter()
                    .map(uqa_sql::routines::invocation::runtime_record_column_type)
                    .collect(),
                Value::Record(fields) => fields
                    .iter()
                    .map(|(_, value)| {
                        uqa_sql::routines::invocation::runtime_record_column_type(value)
                    })
                    .collect(),
                _ => vec![uqa_sql::routines::invocation::runtime_record_column_type(
                    value,
                )],
            };
            &inferred
        };
        uqa_sql::routines::result_check::validate_anonymous_record_result(
            target.catalog,
            source,
            &target.columns,
            None,
        )
    }
    pub(super) fn datum_type(&self, index: usize) -> Option<ColumnType> {
        self.resolver().datum_type(index)
    }

    pub(super) fn record_expression_schema(
        &self,
        expression: &PLpgSQLExpression,
    ) -> Result<Option<uqa_sql::RowSchema>, SQLError> {
        let prepared = self.prepare_expression(expression)?;
        if let Some(schema) = self.raw_record_expression_schema(prepared.expression()?)? {
            return Ok(Some(schema));
        }
        if let Some(fields) = prepared.analysis.result.record_fields(0) {
            return Ok(Some(uqa_sql::RowSchema::with_types(
                (1..=fields.len()).map(|i| format!("f{i}")).collect(),
                fields.to_vec(),
            )));
        }
        Ok(None)
    }

    fn raw_record_expression_schema(
        &self,
        expr: &Expr,
    ) -> Result<Option<uqa_sql::RowSchema>, SQLError> {
        match expr {
            Expr::Column(name) | Expr::QualifiedStar(name) => Ok(self
                .resolver()
                .lookup(name)
                .and_then(|index| self.record_schemas.get(&index).cloned())),
            Expr::Cast { expr, ty, .. }
                if matches!(
                    self.services.expressions.catalog_column_type(ty),
                    Some(ColumnType::Record)
                ) =>
            {
                self.raw_record_expression_schema(expr)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn return_record_types(
        &self,
        value: &PLpgSQLReturnValue,
    ) -> Result<Option<Vec<Option<ColumnType>>>, SQLError> {
        match value {
            PLpgSQLReturnValue::Expr(expr) => Ok(self
                .record_expression_schema(expr)?
                .map(|schema| schema.column_types().to_vec())),
            PLpgSQLReturnValue::Datum(index) => match &self.datums[*index] {
                PLpgSQLDatum::Row { fields } => Ok(Some(
                    fields
                        .iter()
                        .map(|field| self.datum_type(field.varno))
                        .collect(),
                )),
                _ => Ok(self
                    .record_schemas
                    .get(index)
                    .map(|schema| schema.column_types().to_vec())),
            },
        }
    }

    pub(super) fn return_value_type(
        &self,
        value: &PLpgSQLReturnValue,
    ) -> Result<Option<ColumnType>, SQLError> {
        match value {
            PLpgSQLReturnValue::Expr(expr) => Ok(self
                .prepare_expression(expr)?
                .analysis
                .result
                .column_types()
                .and_then(|types| types.first())
                .cloned()
                .flatten()),
            PLpgSQLReturnValue::Datum(index) => Ok(self.datum_type(*index)),
        }
    }
}

pub fn shape_trigger_outcome(
    outcome: RoutineOutcome,
    context: &TriggerRoutineContext,
    catalog: &dyn uqa_sql::routines::declaration::RoutineTypeCatalog,
) -> Result<Value, SQLError> {
    let values = match outcome.value {
        Value::Null => return Ok(Value::Null),
        Value::Record(fields) => fields.into_values(),
        Value::Row(values) => values.into_values(),
        _ => return Err(trigger_shape_error()),
    };
    let types = outcome
        .anonymous_record_column_types
        .unwrap_or_else(|| values.iter().map(runtime_record_column_type).collect());
    if values.len() != types.len() {
        return Err(trigger_shape_error());
    }
    let fields = match (&context.new, &context.old) {
        (Value::Record(fields), _) | (_, Value::Record(fields)) => fields,
        _ => return Err(trigger_shape_error()),
    };
    if fields.len() != context.row_schema.len() {
        return Err(trigger_shape_error());
    }
    let target = fields
        .iter()
        .zip(context.row_schema.column_types())
        .map(|((name, _), ty)| {
            Ok(uqa_sql::routines::result_check::SQLFunctionResultColumn {
                name: name.clone(),
                ty: ty.clone().ok_or_else(trigger_shape_error)?,
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    uqa_sql::routines::result_check::validate_trigger_record_result(catalog, &types, &target)?;
    Ok(Value::Record(uqa_core::RecordValue::from_parts(
        fields
            .iter()
            .map(|(name, _)| name.clone())
            .zip(values)
            .collect(),
        fields.type_oid(),
    )))
}

fn trigger_shape_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42804".into(),
        message: "returned row structure does not match the structure of the triggering table"
            .into(),
    }
}

fn runtime_record_column_type(value: &Value) -> Option<ColumnType> {
    if matches!(value, Value::Null) {
        None
    } else {
        ColumnType::from_sql_name(uqa_sql::expr::value_type_name(value)).ok()
    }
}
