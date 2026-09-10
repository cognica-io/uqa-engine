//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered parameter occurrences and their independently resolved SQL types.

use crate::{ColumnType, SQLError, SQLParam};
use uqa_core::Value;

#[derive(Clone)]
pub(super) struct ExpressionType {
    pub(super) ty: Option<ColumnType>,
    occurrence: Option<usize>,
    // SQL unknown literals and parameters accept context coercion. Native
    // callbacks with no declared result type stay unresolved until execution.
    coercible_unknown: bool,
}

impl ExpressionType {
    pub(super) fn resolved(ty: Option<ColumnType>) -> Self {
        Self {
            ty,
            occurrence: None,
            coercible_unknown: false,
        }
    }

    pub(super) fn unknown() -> Self {
        Self {
            ty: None,
            occurrence: None,
            coercible_unknown: true,
        }
    }

    pub(super) fn is_deferred(&self) -> bool {
        self.ty.is_none() && !self.coercible_unknown
    }
}

pub(super) struct ParameterTypes {
    types: Vec<Option<ColumnType>>,
    occurrences: Vec<(usize, Option<ColumnType>)>,
}

impl ParameterTypes {
    pub(super) fn new(types: &[Option<ColumnType>]) -> Self {
        Self {
            types: types.to_vec(),
            occurrences: Vec::new(),
        }
    }

    pub(super) fn reference(&mut self, number: usize) -> Result<ExpressionType, SQLError> {
        let index = number
            .checked_sub(1)
            .filter(|index| *index < self.types.len())
            .ok_or_else(|| error("42P02", format!("there is no parameter ${number}")))?;
        let ty = self.types[index].clone();
        let occurrence = self.occurrences.len();
        self.occurrences.push((index, ty.clone()));
        Ok(ExpressionType {
            ty,
            occurrence: Some(occurrence),
            coercible_unknown: true,
        })
    }

    pub(super) fn coerce_unknown(
        &mut self,
        expression: &mut ExpressionType,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        if expression.ty.is_some() || !expression.coercible_unknown {
            return Ok(());
        }
        let target = target.without_type_modifiers();
        if let Some(occurrence) = expression.occurrence {
            let (index, observed) = &mut self.occurrences[occurrence];
            if self.types[*index].as_ref().is_some_and(|ty| *ty != target) {
                return Err(error(
                    "42P08",
                    format!("inconsistent types deduced for parameter ${}", *index + 1),
                ));
            }
            self.types[*index] = Some(target.clone());
            *observed = Some(target.clone());
        }
        expression.ty = Some(target);
        Ok(())
    }

    pub(super) fn values(&self) -> Vec<SQLParam> {
        self.types
            .iter()
            .map(|ty| match ty {
                Some(ty) => SQLParam::typed_scalar(Value::Null, ty.clone()),
                None => SQLParam::Scalar(Value::Null),
            })
            .collect()
    }

    pub(super) fn finish(self) -> Result<Vec<Option<ColumnType>>, SQLError> {
        for (index, observed) in self.occurrences {
            if observed != self.types[index] {
                return Err(error(
                    "42P08",
                    format!("could not determine data type of parameter ${}", index + 1),
                ));
            }
        }
        if let Some(index) = self.types.iter().position(Option::is_none) {
            return Err(error(
                "42P18",
                format!("could not determine data type of parameter ${}", index + 1),
            ));
        }
        Ok(self.types)
    }
}

pub(super) fn error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}
