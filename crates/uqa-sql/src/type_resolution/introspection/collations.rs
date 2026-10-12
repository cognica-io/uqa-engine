//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select explicit collation identities without treating the clause as a SQL routine call.

use super::{Binder, BindingCall, LiteralBinding, SQLError, ScalarExpr, Value};
use crate::ast::FunctionDispatch;

impl Binder<'_, '_> {
    pub(super) fn bind_collation(&self, call: &mut BindingCall) -> Result<bool, SQLError> {
        let Some(FunctionDispatch::Collate { oid }) =
            call.binding.as_ref().and_then(|binding| binding.dispatch)
        else {
            return Ok(false);
        };
        let [argument, ScalarExpr::Literal(Value::Str(name))] = call.arguments.as_slice() else {
            return Err(SQLError::Internal("invalid COLLATE expression".into()));
        };
        let selected = (|| {
            let ty = super::super::common::common_context_expression_type_with_control(
                argument,
                self.schema,
                self.params,
                self.resolver,
                &self.control,
            )?;
            crate::catalog::collations::expression::select(name, oid, ty.as_deref(), self.resolver)
        })();
        let selected = if self.literal_binding == LiteralBinding::Ordinary {
            self.semantic(selected)?
        } else {
            Some(selected?)
        };
        if let Some(oid) = selected {
            call.binding.as_mut().expect("collation binding").dispatch =
                Some(FunctionDispatch::Collate { oid: Some(oid) });
        }
        Ok(true)
    }
}
