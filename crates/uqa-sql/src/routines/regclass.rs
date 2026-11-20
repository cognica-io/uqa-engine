//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time relation identities in SQL-standard bodies and argument defaults.

#[cfg(test)]
mod tests;

use super::dependencies::RoutineDeclaration;
use super::{declaration::RoutineTypeCatalog, lifecycle::relations::is_regclass_with_work};
use crate::{
    ast::{ColumnType, CreateFunction, Expr, FunctionParam},
    catalog::{
        stored_ast::{Actions, StoredExpressionRoot},
        work::{self, ReferenceWork, Work},
    },
    SQLError,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MemoryReservation, ProductionControl},
    Value,
};

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
struct RegclassBinding<'a, T: ?Sized, R: ?Sized> {
    types: &'a T,
    relations: &'a R,
    changed: bool,
    memory: Option<MemoryReservation>,
}

impl<'a, T: RoutineTypeCatalog + ?Sized, R: RoutineRegclassCatalog + ?Sized>
    RegclassBinding<'a, T, R>
{
    fn new(types: &'a T, relations: &'a R, control: &ProductionControl<'_>) -> Self {
        Self {
            types,
            relations,
            changed: false,
            memory: control.empty_reservation(),
        }
    }

    async fn regclass_base_type<W: Work + Send>(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<bool, SQLError> {
        let Some(ty) = self
            .types
            .resolve_catalog_column_type_with_control(name, control)?
        else {
            return Ok(false);
        };
        let mut work = ReferenceWork { control, work };
        let base = crate::type_resolution::base_type_with_work(&ty, control, &mut work).await?;
        let result = matches!(base, ColumnType::Regclass);
        ColumnType::retire_with_work(ty, control, &mut work).await?;
        Ok(result)
    }

    async fn literal<W: Work + Send>(
        &mut self,
        mut expression: &mut Expr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        while let Expr::Func { binding, args, .. } = expression {
            if binding.as_ref().and_then(|binding| binding.dispatch)
                != Some(crate::ast::FunctionDispatch::NamedArgument)
            {
                return Ok(());
            }
            ReferenceWork { control, work }.checkpoint().await?;
            let Some(argument) = args.get_mut(1) else {
                return Ok(());
            };
            expression = argument;
        }
        let Expr::Literal(Value::Str(reference)) = expression else {
            return Ok(());
        };
        let Some(oid) = self.relations.resolve_routine_regclass(reference)? else {
            let message = work::text_parts(
                work,
                &["relation \"", reference, "\" does not exist"],
                control,
            )
            .await?;
            return Err(SQLError::admit_routine_message("42P01", message, control)?);
        };
        let (ty, memory) = ColumnType::Regclass
            .catalog_name_with_control(control)?
            .into_parts();
        self.memory = control.combine(self.memory.take(), memory);
        *expression = Expr::TypedLiteral {
            collation_oid: None,
            composite_source: None,
            value: Value::Int(oid),
            ty,
        };
        self.changed = true;
        Ok(())
    }

    async fn bind_node<W: Work + Send>(
        &mut self,
        expression: &mut Expr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        match expression {
            Expr::Cast { expr, ty, .. } if is_regclass_with_work(ty, control, work).await? => {
                self.literal(expr, control, work).await?;
            }
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
                if let Some(argument) = args.first_mut() {
                    self.literal(argument, control, work).await?;
                }
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
                for (argument, target) in args.iter_mut().zip(targets) {
                    ReferenceWork { control, work }.checkpoint().await?;
                    if self.regclass_base_type(target, control, work).await? {
                        self.literal(argument, control, work).await?;
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }

    async fn default<'f, W: Work + Send>(
        &mut self,
        parameter: &'f mut FunctionParam,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<StoredExpressionRoot<'f>>, SQLError> {
        let Some(default) = parameter.default.as_mut() else {
            return Ok(None);
        };
        if self
            .regclass_base_type(&parameter.type_name, control, work)
            .await?
        {
            self.literal(default, control, work).await?;
        }
        Ok(Some(StoredExpressionRoot::Expression(default)))
    }

    fn visit_inline(
        &mut self,
        root: StoredExpressionRoot<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        root.visit_inline(
            |expr| work::inline(self.bind_node(expr, control, &mut ())),
            control,
        )?;
        Ok(self.changed)
    }
}

impl<T: RoutineTypeCatalog + Sync, R: RoutineRegclassCatalog + Sync, W: Work + Send> Actions<W>
    for RegclassBinding<'_, T, R>
{
    async fn expression(
        &mut self,
        expr: &mut Expr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.bind_node(expr, control, work).await
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
        let mut binding = RegclassBinding::new(types, relations, control);
        let edited = if self.parameters_done {
            definition.edit_statement(self.position, control, |statement| {
                binding.visit_inline(StoredExpressionRoot::Statement(statement), control)
            })?
        } else {
            definition.edit_parameter(self.position, control, |parameter| {
                let Some(root) = work::inline(binding.default(parameter, control, &mut ()))? else {
                    return Ok(false);
                };
                binding.visit_inline(root, control)
            })?
        };
        Ok(self.accept(edited))
    }

    /// Bind one field under the private defining catalog, retaining its scratch and locator until the complete edit is published.
    pub async fn advance_from<R: RoutineTypeCatalog + RoutineRegclassCatalog + Sync>(
        &mut self,
        catalog: &R,
        definition: &mut super::StoredRoutineDefinition,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<bool>, SQLError> {
        control.check()?;
        let mut binding = RegclassBinding::new(catalog, catalog, control);
        let changed = if self.parameters_done {
            let Some(mut field) = definition
                .statement_edit_from(self.position, control, input)
                .await?
            else {
                return Ok(self.accept(None));
            };
            StoredExpressionRoot::Statement(field.value_mut())
                .visit_with(&mut binding, control, input)
                .await?;
            field.finish_from(binding.changed, control, input).await?
        } else {
            let Some(mut field) = definition
                .parameter_edit_from(self.position, control, input)
                .await?
            else {
                return Ok(self.accept(None));
            };
            if let Some(root) = binding.default(field.value_mut(), control, input).await? {
                root.visit_with(&mut binding, control, input).await?;
            }
            field.finish_from(binding.changed, control, input).await?
        };
        Ok(self.accept(Some(changed)))
    }

    fn accept(&mut self, edited: Option<bool>) -> Option<bool> {
        if let Some(changed) = edited {
            self.changed |= changed;
            self.position += 1;
        } else if self.parameters_done {
            return Some(self.changed);
        } else {
            self.parameters_done = true;
            self.position = 0;
        }
        None
    }
}
