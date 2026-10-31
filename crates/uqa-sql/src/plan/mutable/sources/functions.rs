//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected function members receive independent admitted destinations while their original range item remains available for rollback.

use super::super::super::copy::Copying;
use super::{empty_source, MutableQueryPlan, MutableSourcePlan, SQLError, SourcePlan};
use uqa_core::memory::ProductionControl;

impl SourcePlan {
    pub fn copy_function_group_member_for_rewrite(
        &self,
        index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<MutableSourcePlan, SQLError> {
        control.check()?;
        let Self::FunctionGroup {
            functions,
            alias,
            ordinality: false,
            ..
        } = self
        else {
            return Err(SQLError::Internal(
                "member expansion requires a function group without ordinality".into(),
            ));
        };
        let Some(function) = functions.get(index) else {
            return Err(SQLError::Internal(
                "function member is outside its source".into(),
            ));
        };
        let output = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let source = copying.function_group_member(function, alias.as_deref())?;
            MutableSourcePlan {
                source,
                memory: Some(copying.into_rewrite_memory()),
            }
        } else {
            let function = function.clone();
            MutableSourcePlan {
                source: SourcePlan::Function {
                    name: function.name,
                    binding: function.binding,
                    output_name: function.output_name,
                    relations: function.relations,
                    args: function.args,
                    alias: alias.clone(),
                    column_aliases: function.column_aliases,
                    column_types: function.column_types,
                    ordinality: false,
                },
                memory: None,
            }
        };
        control.check()?;
        Ok(output)
    }
}

impl MutableSourcePlan {
    /// Select the contained query and retire the former source, including its metadata and query box, before returning the unchanged query constructor owner.
    pub fn into_query(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableQueryPlan, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let field = match &self.source {
            SourcePlan::Subquery { .. } => "body",
            SourcePlan::Table {
                executable_query: Some(_),
                ..
            } => "executable_query",
            _ => {
                return Err(SQLError::Internal(
                    "source does not contain an executable query".into(),
                ));
            }
        };
        let memory = self.memory.as_mut().and_then(|memory| {
            memory
                .field_mut(field)
                .as_mut()
                .expect("query box")
                .field_mut("query")
                .take()
        });
        let source = std::mem::replace(&mut self.source, empty_source());
        let query = match source {
            SourcePlan::Subquery { body, .. }
            | SourcePlan::Table {
                executable_query: Some(body),
                ..
            } => *body,
            _ => unreachable!("selected query source"),
        };
        let output = MutableQueryPlan { query, memory };
        drop(self);
        control.check()?;
        Ok(output)
    }
}
