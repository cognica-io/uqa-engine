//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine parameters in SQL-standard bodies, which `get_parameter` prints by name rather than as `$n`.

use uqa_sql::ast::CreateFunction;

use super::{quote_ident, Deparser, SQLError, Scope};

/// The routine whose body is printed: its name and its parameters' names, as `print_function_sqlbody` sets them up for `get_parameter`.
pub struct RoutineNamespace {
    name: String,
    /// Every declared parameter's name in declaration order, `None` for an unnamed one. A body's `$n` prints as the `n`th, and its input parameters go by the first names.
    names: Vec<Option<String>>,
    /// How many input parameters the body refers to.
    inputs: usize,
}

impl RoutineNamespace {
    pub fn new(def: &CreateFunction) -> Result<Self, SQLError> {
        Ok(Self {
            name: uqa_sql::routines::routine_local_name(&def.name)?,
            names: def
                .params
                .iter()
                .map(|parameter| (!parameter.name.is_empty()).then(|| parameter.name.clone()))
                .collect(),
            inputs: def.sql_body_parameters().len(),
        })
    }
}

impl Deparser<'_> {
    /// A column reference that names one of the routine's input parameters and no column of any query level, which the body's parser read as that parameter.
    pub(super) fn parameter_reference(
        &self,
        qualifier: Option<&str>,
        name: &str,
        scope: &Scope,
    ) -> Option<String> {
        let routine = self.routine.as_ref()?;
        if qualifier.is_some_and(|qualifier| qualifier != routine.name)
            || scope.resolves(qualifier, name)
        {
            return None;
        }
        let position = routine.names[..routine.inputs.min(routine.names.len())]
            .iter()
            .position(|parameter| parameter.as_deref() == Some(name))?;
        Some(self.parameter(position + 1, scope))
    }

    /// A column reference: a column of a query level, or else one of the routine's parameters.
    pub(super) fn column_reference(
        &self,
        qualifier: Option<&str>,
        name: &str,
        scope: &Scope,
    ) -> String {
        self.parameter_reference(qualifier, name, scope)
            .unwrap_or_else(|| scope.column(qualifier, name))
    }

    /// `get_parameter`: `$n` prints as the name of the routine's `n`th parameter, qualified by the routine's name when a query level has a range table whose columns the name could otherwise denote.
    pub(super) fn parameter(&self, number: usize, scope: &Scope) -> String {
        let named = self.routine.as_ref().and_then(|routine| {
            let name = routine.names.get(number.checked_sub(1)?)?.as_ref()?;
            Some((routine, name))
        });
        match named {
            Some((routine, name)) if scope.range_table => {
                format!("{}.{}", quote_ident(&routine.name), quote_ident(name))
            }
            Some((_, name)) => quote_ident(name),
            None => format!("${number}"),
        }
    }
}

pub(super) struct StoredMergeColumns<'a> {
    pub catalog: &'a crate::catalog::CatalogReadView,
    pub resolution: &'a crate::catalog::RelationNameResolution,
}

impl uqa_sql::routines::merge_columns::StoredMergeColumnCatalog for StoredMergeColumns<'_> {
    fn stored_merge_target_definitions(&self, table: &str) -> Option<Vec<uqa_sql::ast::ColumnDef>> {
        self.catalog
            .table(self.resolution, table)
            .ok()
            .flatten()
            .map(|table| table.columns.as_ref().clone())
    }
}
