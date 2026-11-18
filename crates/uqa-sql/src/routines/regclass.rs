//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time relation identities in SQL-standard bodies and argument defaults.

use super::dependencies::RoutineDeclaration;
use super::{declaration::RoutineTypeCatalog, lifecycle::relations::is_regclass};
use crate::{
    ast::{ColumnType, CreateFunction, Expr},
    SQLError,
};
use uqa_core::{memory::ProductionControl, Value};

pub trait RoutineRegclassCatalog {
    fn resolve_routine_regclass(&self, reference: &str) -> Result<Option<i64>, SQLError>;
}
pub fn bind_routine_regclass_constants(
    types: &dyn RoutineTypeCatalog,
    relations: &dyn RoutineRegclassCatalog,
    definition: &mut CreateFunction,
) -> Result<bool, SQLError> {
    let mut binding = RoutineRegclassBinding::default();
    loop {
        if let Some(changed) = binding.advance(
            types,
            relations,
            definition,
            &ProductionControl::uncontrolled(),
        )? {
            return Ok(changed);
        }
    }
}
struct RegclassBinding<'a> {
    types: &'a dyn RoutineTypeCatalog,
    relations: &'a dyn RoutineRegclassCatalog,
}
impl RegclassBinding<'_> {
    fn regclass_base_type(&self, name: &str) -> bool {
        let Some(ty) = self.types.resolve_catalog_column_type(name) else {
            return false;
        };
        let mut ty = &ty;
        while let ColumnType::Domain { base, .. } = ty {
            ty = base;
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
            collation_oid: None,
            composite_source: None,
            value: Value::Int(oid),
            ty: ColumnType::Regclass.catalog_name(),
        };
        Ok(true)
    }

    fn bind_regclass_expression(&self, expression: &mut Expr) -> Result<bool, SQLError> {
        match expression {
            Expr::Cast { expr, ty, .. } if is_regclass(ty) => self.bind_regclass_literal(expr),
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
}

#[derive(Default)]
pub struct RoutineRegclassBinding {
    parameters_done: bool,
    position: usize,
    changed: bool,
}

impl RoutineRegclassBinding {
    pub fn advance(
        &mut self,
        types: &dyn RoutineTypeCatalog,
        relations: &dyn RoutineRegclassCatalog,
        definition: &mut impl RoutineDeclaration,
        control: &ProductionControl<'_>,
    ) -> Result<Option<bool>, SQLError> {
        control.check()?;
        let binding = RegclassBinding { types, relations };
        let mut changed = false;
        if !self.parameters_done {
            if let Some(result) =
                definition.edit_parameter(self.position, control, |parameter| {
                    if let Some(default) = &mut parameter.default {
                        if binding.regclass_base_type(&parameter.type_name) {
                            changed |= binding.bind_regclass_literal(default)?;
                        }
                        crate::catalog::stored_ast::visit_stored_expression(
                            default,
                            &mut |expression| {
                                changed |= binding.bind_regclass_expression(expression)?;
                                Ok(())
                            },
                        )?;
                    }
                    Ok(changed)
                })?
            {
                self.changed |= result;
                self.position += 1;
            } else {
                self.position = 0;
                self.parameters_done = true;
            }
        } else if let Some(result) =
            definition.edit_statement(self.position, control, |statement| {
                crate::catalog::stored_ast::visit_stored_statement_expressions(
                    statement,
                    &mut |expression| {
                        changed |= binding.bind_regclass_expression(expression)?;
                        Ok(())
                    },
                )?;
                Ok(changed)
            })?
        {
            self.changed |= result;
            self.position += 1;
        } else {
            return Ok(Some(self.changed));
        }
        Ok(None)
    }
}
