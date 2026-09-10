//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time relation identities in SQL-standard bodies and argument defaults.

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, CreateFunction, Expr, FunctionBody};

use super::{Engine, SQLError};

pub(super) fn is_regclass(name: &str) -> bool {
    name.eq_ignore_ascii_case("regclass") || name.eq_ignore_ascii_case("pg_catalog.regclass")
}

pub(super) fn regclass_oid(expression: &Expr) -> Option<i64> {
    match expression {
        Expr::TypedLiteral {
            value: Value::Int(oid),
            ty,
        } if is_regclass(ty) => Some(*oid),
        Expr::Cast { expr, ty } if is_regclass(ty) => regclass_oid(expr),
        _ => None,
    }
}

impl Engine {
    fn regclass_base_type(&self, name: &str) -> bool {
        let Some(mut ty) = crate::sql::resolve_catalog_column_type(self, name) else {
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
                == Some(uqa_sql::ast::FunctionDispatch::NamedArgument)
            {
                return args
                    .get_mut(1)
                    .map_or(Ok(false), |argument| self.bind_regclass_literal(argument));
            }
        }
        let Expr::Literal(Value::Str(reference)) = expression else {
            return Ok(false);
        };
        let oid = crate::sql::resolve_regclass_oid(self, reference)?.ok_or_else(|| {
            SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{reference}\" does not exist"),
            }
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

    pub(super) fn bind_routine_regclass_constants(
        &self,
        definition: &mut CreateFunction,
    ) -> Result<bool, SQLError> {
        let previous = {
            let mut state = self.session.state.write();
            std::mem::replace(
                &mut state.search_path,
                definition.creation_search_path.clone(),
            )
        };
        let result = self.bind_routine_regclass_constants_at_search_path(definition);
        self.session.state.write().search_path = previous;
        result
    }

    fn bind_routine_regclass_constants_at_search_path(
        &self,
        definition: &mut CreateFunction,
    ) -> Result<bool, SQLError> {
        let mut changed = false;
        for parameter in &mut definition.params {
            if let Some(default) = &mut parameter.default {
                if self.regclass_base_type(&parameter.type_name) {
                    changed |= self.bind_regclass_literal(default)?;
                }
                crate::engine_events::visit_stored_expression(default, &mut |expression| {
                    changed |= self.bind_regclass_expression(expression)?;
                    Ok(())
                })?;
            }
        }
        if let FunctionBody::Statements(statements) = &mut definition.body {
            for statement in statements {
                crate::engine_events::visit_stored_statement_expressions(
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
