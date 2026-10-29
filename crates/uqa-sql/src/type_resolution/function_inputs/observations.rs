//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow only the original call inputs while sharing existing type and signature validation.

use crate::{ast::InternalColumnRef, schema::ScalarTypeSchema, ColumnType, RowSchema, ScalarExpr};

pub(crate) struct CallInputTypes<'a> {
    schema: &'a dyn ScalarTypeSchema,
    observations: Vec<(&'a ScalarExpr, Option<&'a ColumnType>)>,
}

impl<'a> CallInputTypes<'a> {
    pub(crate) fn new_with_control(
        schema: &'a dyn ScalarTypeSchema,
        observations: impl Iterator<Item = (&'a ScalarExpr, Option<&'a ColumnType>)>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>, crate::SQLError> {
        let mut output = uqa_core::memory::ProductionVec::new(*control);
        output.reserve(observations.size_hint().0)?;
        for observation in observations {
            output.push_copy(observation)?;
        }
        let mut output = output.finish()?;
        output
            .as_mut_slice()
            .sort_unstable_by_key(|(expression, _)| identity(expression));
        let (observations, memory) = output.into_parts();
        Ok(control.finish(
            Self {
                schema,
                observations,
            },
            memory,
        )?)
    }
}

fn identity(expression: &ScalarExpr) -> usize {
    std::ptr::from_ref(expression).addr()
}

impl ScalarTypeSchema for CallInputTypes<'_> {
    fn input_collation(&self, expression: &ScalarExpr) -> Option<u32> {
        self.schema.input_collation(expression)
    }

    fn observed_expression_type(&self, expression: &ScalarExpr) -> Option<Option<&ColumnType>> {
        let argument = crate::scalar_call_argument(expression).ok()?.value;
        self.observations
            .binary_search_by_key(&identity(argument), |(expression, _)| identity(expression))
            .ok()
            .map(|position| self.observations[position].1)
    }

    fn has_unqualified_column(&self, name: &str) -> bool {
        self.schema.has_unqualified_column(name)
    }
    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.schema.column_is_ambiguous(name)
    }
    fn type_of(&self, name: &str) -> Option<&ColumnType> {
        self.schema.type_of(name)
    }
    fn column_type(&self, position: usize) -> Option<&ColumnType> {
        self.schema.column_type(position)
    }
    fn internal_type(&self, column: InternalColumnRef) -> Option<&ColumnType> {
        self.schema.internal_type(column)
    }
    fn has_qualifier(&self, qualifier: &str) -> bool {
        self.schema.has_qualifier(qualifier)
    }
    fn whole_row_type(&self, qualifier: &str) -> Option<&ColumnType> {
        self.schema.whole_row_type(qualifier)
    }
    fn has_qualified_column(&self, qualifier: &str, name: &str) -> bool {
        self.schema.has_qualified_column(qualifier, name)
    }
    fn qualified_column_is_ambiguous(&self, qualifier: &str, name: &str) -> bool {
        self.schema.qualified_column_is_ambiguous(qualifier, name)
    }
    fn qualified_type(&self, qualifier: &str, name: &str) -> Option<&ColumnType> {
        self.schema.qualified_type(qualifier, name)
    }
    fn columns_are_open(&self, qualifier: Option<&str>) -> bool {
        self.schema.columns_are_open(qualifier)
    }
    fn column_collation(&self, position: usize) -> Option<u32> {
        self.schema.column_collation(position)
    }
    fn collation_of(&self, name: &str) -> Option<u32> {
        self.schema.collation_of(name)
    }
    fn qualified_collation(&self, qualifier: &str, name: &str) -> Option<u32> {
        self.schema.qualified_collation(qualifier, name)
    }
    fn internal_collation(&self, column: InternalColumnRef) -> Option<u32> {
        self.schema.internal_collation(column)
    }
    fn physical_schema(&self) -> Option<&RowSchema> {
        self.schema.physical_schema()
    }
}
