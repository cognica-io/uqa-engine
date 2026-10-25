//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and admitted vector calls share argument validation and native vector construction.

use super::{bad_operator_arity, RetrievalConstants, RetrievalExpr, SQLError, ScalarExpr};
use crate::{retrieval::ControlledConstantEvaluator, SQLParam};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub(super) fn ordinary(
    calibrated: bool,
    args: &[ScalarExpr],
    constants: &RetrievalConstants<'_>,
) -> Result<RetrievalExpr, SQLError> {
    bind(
        calibrated,
        args,
        constants.params,
        &|expr, params, control| {
            Ok(control.retain_external_value((constants.evaluate)(expr, params)?)?)
        },
        &ProductionControl::uncontrolled(),
    )
    .map(|value| value.into_uncontrolled().expect("ordinary vector call"))
}

pub(crate) fn resource_failure(error: &SQLError) -> bool {
    matches!(error.sqlstate(), Some("53200" | "57014"))
}

fn value(
    expression: &ScalarExpr,
    params: &[SQLParam],
    evaluate: &ControlledConstantEvaluator<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    control.check()?;
    match evaluate(expression, params, control) {
        Ok(value) => Ok(Some(value)),
        Err(error) if resource_failure(&error) => Err(error),
        Err(_) => Ok(None),
    }
}

pub(crate) fn field(
    expression: &ScalarExpr,
    calibrated: bool,
    params: &[SQLParam],
    evaluate: &ControlledConstantEvaluator<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>, SQLError> {
    match expression {
        ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. } => {
            return Ok(Some(control.copy_text(name)?));
        }
        _ if !calibrated => return Ok(None),
        _ => {}
    }
    let Some(value) = value(expression, params, evaluate, control)? else {
        return Ok(None);
    };
    if !matches!(&*value, Value::Str(_)) {
        return Ok(None);
    }
    let (Value::Str(text), memory) = value.into_parts() else {
        unreachable!()
    };
    Ok(Some(control.finish(text, memory)?))
}

fn numeric(value: &Value, control: &ProductionControl<'_>) -> Result<Option<f64>, SQLError> {
    control.check()?;
    Ok(match value {
        Value::Int(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        Value::Decimal(value) => value.to_f64_with_control(control)?,
        _ => None,
    })
}

fn vector(
    expression: &ScalarExpr,
    params: &[SQLParam],
    evaluate: &ControlledConstantEvaluator<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<f32>>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    if let ScalarExpr::Array(items) = expression {
        for item in items {
            let Some(value) = value(item, params, evaluate, control)? else {
                return Ok(None);
            };
            let Some(component) = numeric(&value, control)? else {
                return Ok(None);
            };
            output.extend_from_slice(&[component as f32])?;
        }
    } else {
        let Some(value) = value(expression, params, evaluate, control)? else {
            return Ok(None);
        };
        let items = match &*value {
            Value::Array(array) if array.dimensions().len() <= 1 => array.elements(),
            Value::List(items) => items,
            _ => return Ok(None),
        };
        for item in items {
            let Some(component) = numeric(item, control)? else {
                return Ok(None);
            };
            output.extend_from_slice(&[component as f32])?;
        }
    }
    Ok(Some(output.finish()?))
}

pub(crate) fn bind(
    calibrated: bool,
    args: &[ScalarExpr],
    params: &[SQLParam],
    evaluate: &ControlledConstantEvaluator<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<RetrievalExpr>, SQLError> {
    control.check()?;
    let name = if calibrated {
        "calibrated_vector_match"
    } else {
        "knn_match"
    };
    if !(if calibrated {
        (3..=4).contains(&args.len())
    } else {
        args.len() == 3
    }) {
        return Err(bad_operator_arity(
            name,
            if calibrated { "3..=4" } else { "3" },
            args.len(),
        ));
    }
    let field = field(&args[0], calibrated, params, evaluate, control)?.ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{name}.field must be a {}",
            if calibrated {
                "column reference or constant string"
            } else {
                "column reference"
            }
        ))
    })?;
    if field.trim().is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "{name}.field cannot be empty"
        )));
    }
    let vector = vector(&args[1], params, evaluate, control)?.ok_or_else(|| {
        SQLError::TypeMismatch(format!("{name}.vector must be a constant numeric vector"))
    })?;
    if vector.is_empty() || vector.iter().any(|component| !component.is_finite()) {
        return Err(SQLError::TypeMismatch(format!(
            "{name}.vector must be non-empty and contain only finite values"
        )));
    }
    let k = value(&args[2], params, evaluate, control)?
        .and_then(|value| match &*value {
            Value::Int(value) if *value >= 0 => usize::try_from(*value).ok(),
            _ => None,
        })
        .ok_or_else(|| {
            SQLError::TypeMismatch(format!("{name}.k must be a non-negative integer"))
        })?;
    if k == 0 || i64::try_from(k).is_err() {
        return Err(SQLError::TypeMismatch(format!(
            "{name}.k must be positive and fit in a SQL BIGINT, got {k}"
        )));
    }
    let threshold = if let Some(argument) = args.get(3).filter(|_| calibrated) {
        let number = value(argument, params, evaluate, control)?
            .map(|value| numeric(&value, control))
            .transpose()?
            .flatten();
        Some(number.ok_or_else(|| {
            SQLError::TypeMismatch(
                "calibrated_vector_match.threshold must be a constant number".into(),
            )
        })?)
    } else {
        None
    };
    if let Some(threshold) =
        threshold.filter(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(SQLError::TypeMismatch(format!(
            "calibrated_vector_match.threshold must be finite and in [0, 1], got {threshold}"
        )));
    }
    let (field, field_memory) = field.into_parts();
    let (query_vector, vector_memory) = vector.into_parts();
    let call = if calibrated {
        RetrievalExpr::CalibratedVectorMatch {
            field,
            query_vector,
            k,
            threshold,
        }
    } else {
        RetrievalExpr::KNN {
            field,
            query_vector,
            k,
        }
    };
    Ok(control.finish(call, control.combine(field_memory, vector_memory))?)
}
