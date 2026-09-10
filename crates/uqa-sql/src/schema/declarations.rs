//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declaration validation and binding for stored schema expressions.

use crate::{
    ast::{ColumnDef, FunctionVolatility},
    SQLError,
};
use crate::{expr::EngineHook, semantics::sets::SetFunctionCatalog};

/// Definition lookup for generated columns and immutable index expressions.
pub trait SchemaExpressionCatalog: EngineHook + SetFunctionCatalog {
    fn registered_runtime_function_volatility(&self, name: &str) -> Option<FunctionVolatility>;
    fn schema_expression_columns(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, SQLError>;
}

/// Routine metadata and immutable namespace inputs for schema expression binding.
pub struct SchemaBindingContext<'a, 'q> {
    pub catalog: &'a dyn SchemaExpressionCatalog,
    pub binding: &'a crate::binding::context::BindingContext<'q>,
}
