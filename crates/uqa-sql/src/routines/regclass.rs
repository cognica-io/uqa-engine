//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time relation identities in SQL-standard bodies and argument defaults.

use super::{declaration::RoutineTypeCatalog, lifecycle::relations::is_regclass};
use crate::{
    ast::{ColumnType, CreateFunction, Expr, FunctionBody},
    SQLError,
};
use uqa_core::Value;

pub trait RoutineRegclassCatalog {
    fn resolve_routine_regclass(&self, reference: &str) -> Result<Option<i64>, SQLError>;
}
pub fn bind_routine_regclass_constants(
    types: &dyn RoutineTypeCatalog,
    relations: &dyn RoutineRegclassCatalog,
    definition: &mut CreateFunction,
) -> Result<bool, SQLError> {
    RegclassBinding { types, relations }.bind_routine_constants(definition)
}
struct RegclassBinding<'a> {
    types: &'a dyn RoutineTypeCatalog,
    relations: &'a dyn RoutineRegclassCatalog,
}
impl RegclassBinding<'_> {
    fn regclass_base_type(&self, name: &str) -> bool {
        let Some(mut ty) = self.types.resolve_catalog_column_type(name) else {
            return false;
        };
        while let ColumnType::Domain { base, .. } = ty {
            ty = *base;
        }
        matches!(ty, ColumnType::Regclass)
    }

    fn bind_regclass_literal(&self, expression: &mut Expr) -> Result<bool, SQLError> {
        if let Expr::Func { binding, args, .. } = expression {
            if binding.as_ref().and_then(|binding| binding.dispatch)
                == Some(crate::ast::FunctionDispatch::NamedArgument)
            {
                return args
                    .get_mut(1)
                    .map_or(Ok(false), |argument| self.bind_regclass_literal(argument));
            }
        }
        let Expr::Literal(Value::Str(reference)) = expression else {
            return Ok(false);
        };
        let oid = self
            .relations
            .resolve_routine_regclass(reference)?
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{reference}\" does not exist"),
            })?;
        *expression = Expr::TypedLiteral {
            value: Value::Int(oid),
            ty: "regclass".into(),
        };
        Ok(true)
    }

    fn bind_regclass_expression(&self, expression: &mut Expr) -> Result<bool, SQLError> {
        match expression {
            Expr::Cast { expr, ty } if is_regclass(ty) => self.bind_regclass_literal(expr),
            Expr::Func {
                name,
                binding,
                args,
                ..
            } if binding.as_ref().is_none_or(|binding| binding.builtin)
                && matches!(
                    name.strip_prefix("pg_catalog.").unwrap_or(name),
                    "nextval" | "currval" | "setval"
                ) =>
            {
                args.first_mut()
                    .map_or(Ok(false), |argument| self.bind_regclass_literal(argument))
            }
            Expr::Func {
                binding: Some(binding),
                args,
                ..
            } => {
                let targets = binding
                    .invocation
                    .as_ref()
                    .map_or(binding.argument_types.as_slice(), |invocation| {
                        invocation.argument_targets.as_slice()
                    });
                let mut changed = false;
                for (argument, target) in args.iter_mut().zip(targets) {
                    if self.regclass_base_type(target) {
                        changed |= self.bind_regclass_literal(argument)?;
                    }
                }
                Ok(changed)
            }
            _ => Ok(false),
        }
    }

    fn bind_routine_constants(&self, definition: &mut CreateFunction) -> Result<bool, SQLError> {
        let mut changed = false;
        for parameter in &mut definition.params {
            if let Some(default) = &mut parameter.default {
                if self.regclass_base_type(&parameter.type_name) {
                    changed |= self.bind_regclass_literal(default)?;
                }
                crate::catalog::stored_ast::visit_stored_expression(default, &mut |expression| {
                    changed |= self.bind_regclass_expression(expression)?;
                    Ok(())
                })?;
            }
        }
        if let FunctionBody::Statements(statements) = &mut definition.body {
            for statement in statements {
                crate::catalog::stored_ast::visit_stored_statement_expressions(
                    statement,
                    &mut |expression| {
                        changed |= self.bind_regclass_expression(expression)?;
                        Ok(())
                    },
                )?;
            }
        }
        Ok(changed)
    }
}
