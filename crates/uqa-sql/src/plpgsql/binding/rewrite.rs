//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed expression-level variable references preserve the resolver's original dispatch.

use super::{Expr, Result, VariableResolver};

#[derive(Clone, Copy)]
pub enum RewriteReference<'a> {
    Name(&'a str),
    Qualified { qualifier: &'a str, column: &'a str },
    WholeRow(&'a str),
    Parameter(usize),
    Internal(crate::ast::InternalColumnRef),
}

impl RewriteReference<'_> {
    pub(super) fn resolve(
        self,
        resolver: &mut (impl VariableResolver + ?Sized),
    ) -> Result<Option<Expr>> {
        match self {
            Self::Name(name) => resolver.rewrite_name(name),
            Self::Qualified { qualifier, column } => resolver.rewrite_qualified(qualifier, column),
            Self::WholeRow(qualifier) => resolver.rewrite_qualified_whole_row(qualifier),
            Self::Parameter(index) => resolver.rewrite_param(index),
            Self::Internal(column) => resolver.rewrite_internal(column),
        }
    }
}
