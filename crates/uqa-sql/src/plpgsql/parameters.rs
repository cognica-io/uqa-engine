//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind procedural variables as invocation parameters, preserving catalog inputs.

use super::{
    binding::{bind_statement, ResolvedVariable, VariableResolver},
    variable_conflicts::{statement_variable_names, VariableSiteResolver},
    VariableConflict,
};
use crate::{
    ast::{Expr, Statement},
    SQLError, SQLParam,
};

pub fn parameter_type_mismatch(
    number: usize,
    actual: &crate::ColumnType,
    expected: &crate::ColumnType,
) -> crate::ast::DeferredSQLError {
    crate::ast::DeferredSQLError {
        sqlstate: "42804".into(),
        message: format!(
            "type of parameter {number} ({}) does not match that when preparing the plan ({})",
            actual.display_name(),
            expected.display_name()
        ),
    }
}

#[derive(Debug, Clone)]
pub enum PLpgSQLVariableReference {
    Name(String),
    Qualified { qualifier: String, column: String },
    Parameter(usize),
}
impl PLpgSQLVariableReference {
    pub fn read(&self, resolver: &mut dyn VariableResolver) -> Result<SQLParam, SQLError> {
        let value = match self {
            Self::Name(name) => resolver.resolve_name(name),
            Self::Qualified { qualifier, column } => resolver.resolve_qualified(qualifier, column),
            Self::Parameter(index) => resolver.resolve_param(*index),
        }?
        .ok_or_else(|| {
            SQLError::Internal("prepared PL/pgSQL variable no longer resolves".into())
        })?;
        Ok(parameter(value, resolver))
    }
}
fn parameter(value: ResolvedVariable, resolver: &dyn VariableResolver) -> SQLParam {
    match value
        .declared_type
        .as_deref()
        .and_then(|name| resolver.parameter_type(name))
    {
        Some(ty) => SQLParam::typed_scalar(value.value, ty),
        None => SQLParam::scalar(value.value),
    }
}

pub struct PLpgSQLVariableBindings {
    pub statement: Statement,
    pub references: Vec<PLpgSQLVariableReference>,
    pub parameters: Vec<SQLParam>,
}

/// Reuse the ordinary PL name/column conflict resolver, then number only names
/// it selected as variables. Repeated reads receive fresh invocation values.
pub fn parameterize_statement_variables(
    statement: &Statement,
    resolver: &mut dyn VariableResolver,
    conflict: VariableConflict,
    resolve_sites: VariableSiteResolver<'_>,
) -> Result<PLpgSQLVariableBindings, SQLError> {
    let keep = statement_variable_names(statement, resolver, conflict, resolve_sites)?;
    let mut binding = VariableBindings {
        inner: resolver,
        keep: &keep,
        next_name: 0,
        references: Vec::new(),
        parameters: Vec::new(),
    };
    let statement = bind_statement(statement, &mut binding)?;
    Ok(PLpgSQLVariableBindings {
        statement,
        references: binding.references,
        parameters: binding.parameters,
    })
}
struct VariableBindings<'a> {
    inner: &'a mut dyn VariableResolver,
    keep: &'a [bool],
    next_name: usize,
    references: Vec<PLpgSQLVariableReference>,
    parameters: Vec<SQLParam>,
}
impl VariableBindings<'_> {
    fn mark(
        &mut self,
        reference: PLpgSQLVariableReference,
        value: Option<ResolvedVariable>,
        named: bool,
    ) -> Option<Expr> {
        let value = value?;
        if named {
            let site = self.next_name;
            self.next_name += 1;
            if self.keep.get(site).copied().unwrap_or(false) {
                return None;
            }
        }
        self.references.push(reference);
        self.parameters.push(parameter(value, self.inner));
        Some(Expr::Param(self.references.len()))
    }
}
impl VariableResolver for VariableBindings<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        self.inner.resolve_name(name)
    }
    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.inner.resolve_qualified(qualifier, column)
    }
    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        self.inner.resolve_param(index)
    }
    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        let value = self.inner.resolve_name(name)?;
        Ok(self.mark(
            PLpgSQLVariableReference::Name(name.to_string()),
            value,
            true,
        ))
    }
    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        let value = self.inner.resolve_qualified(qualifier, column)?;
        Ok(self.mark(
            PLpgSQLVariableReference::Qualified {
                qualifier: qualifier.to_string(),
                column: column.to_string(),
            },
            value,
            true,
        ))
    }
    fn rewrite_param(&mut self, index: usize) -> Result<Option<Expr>, SQLError> {
        let value = self.inner.resolve_param(index)?;
        Ok(self.mark(PLpgSQLVariableReference::Parameter(index), value, false))
    }
}

#[cfg(test)]
mod tests;
