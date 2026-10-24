//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pass selected SQL-owned descriptions through existing routine boundaries.

use super::{ColumnType, Interpreter, PLpgSQLFunction, SQLError, Value};
use uqa_sql::routines::invocation::descriptions::{
    ProceduralRoutineDescription, RoutineCallDescription, RoutineConversionEnvironment,
};

#[derive(Clone, Copy)]
pub(crate) struct RoutineBodyDescriptions<'a> {
    pub description: &'a RoutineCallDescription,
    pub environment: RoutineConversionEnvironment<'a>,
}

#[derive(Clone, Copy)]
pub(super) struct ProceduralDescriptions<'a> {
    pub description: ProceduralRoutineDescription<'a>,
    pub environment: RoutineConversionEnvironment<'a>,
}

impl<'a> Interpreter<'a> {
    pub(super) fn set_descriptions(
        &mut self,
        parsed: &PLpgSQLFunction,
        descriptions: Option<RoutineBodyDescriptions<'a>>,
    ) -> Result<(), SQLError> {
        self.descriptions = match descriptions {
            Some(descriptions) => descriptions
                .description
                .procedural_body(parsed, descriptions.environment)?
                .map(|description| ProceduralDescriptions {
                    description,
                    environment: descriptions.environment,
                }),
            None => None,
        };
        Ok(())
    }

    pub(super) fn coerce_datum(
        &self,
        index: usize,
        name: &str,
        value: &Value,
        source: Option<&ColumnType>,
    ) -> Result<Value, SQLError> {
        match self.descriptions {
            Some(descriptions) => descriptions.description.coerce_datum(
                index,
                name,
                value,
                source,
                descriptions.environment,
            ),
            None => {
                super::coerce_routine_value_from(self.services.expressions, value, name, source)
            }
        }
    }

    pub(super) fn coerce_return(
        &self,
        name: &str,
        value: &Value,
        source: Option<&ColumnType>,
    ) -> Result<Value, SQLError> {
        match self.descriptions {
            Some(descriptions) => descriptions.description.coerce_result(
                name,
                value,
                source,
                descriptions.environment,
            ),
            None => {
                super::coerce_routine_value_from(self.services.expressions, value, name, source)
            }
        }
    }
}
