//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedural parameters borrow syntax and admit only selected variable inputs and the final executable.

use super::{
    parameterize_statement_variables, Expr, PLpgSQLVariableReference, SQLError, SQLParam,
    Statement, VariableConflict, VariableResolver,
};
use crate::{
    binding::VariableSiteResolution,
    plan::{MutableExecutablePlan, UnifiedPlan},
    ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

pub struct PLpgSQLInput {
    pub parameter: SQLParam,
    pub collation_oid: Option<u32>,
}

impl PLpgSQLInput {
    pub fn from_parts(
        value: Produced<uqa_core::Value>,
        ty: Option<Produced<crate::ColumnType>>,
        collation_oid: Option<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let (value, mut memory) = value.into_parts();
        let parameter = if let Some(ty) = ty {
            let (ty, lease) = ty.into_parts();
            memory = control.combine(memory, lease);
            SQLParam::typed_scalar(value, ty)
        } else {
            SQLParam::scalar(value)
        };
        Ok(control.finish(
            Self {
                parameter,
                collation_oid,
            },
            memory,
        )?)
    }
}

pub struct OwnedPLpgSQLVariableBindings {
    pub plan: MutableExecutablePlan,
    pub references: Produced<Vec<PLpgSQLVariableReference>>,
    pub parameters: Produced<Vec<SQLParam>>,
}

pub type OwnedVariableSiteResolver<'a> =
    &'a mut dyn FnMut(
        &MutableExecutablePlan,
        &[SQLParam],
        Produced<Vec<ScalarExpr>>,
        &ProductionControl<'_>,
    ) -> Result<Produced<Vec<VariableSiteResolution>>, SQLError>;

impl PLpgSQLVariableReference {
    fn from_expression(
        expression: &Expr,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, SQLError> {
        Ok(Some(match expression {
            Expr::Column(name) => {
                let (name, memory) = control.copy_text(name)?.into_parts();
                control.finish(Self::Name(name), memory)?
            }
            Expr::QualifiedColumn { qualifier, column } => {
                let (qualifier, first) = control.copy_text(qualifier)?.into_parts();
                let (column, second) = control.copy_text(column)?.into_parts();
                control.finish(
                    Self::Qualified { qualifier, column },
                    control.combine(first, second),
                )?
            }
            Expr::Param(index) => {
                control.finish(Self::Parameter(*index), control.empty_reservation())?
            }
            _ => return Ok(None),
        }))
    }
}

pub fn parameterize_statement_variables_owned(
    statement: &Statement,
    resolver: &mut dyn VariableResolver,
    conflict: VariableConflict,
    resolve_sites: OwnedVariableSiteResolver<'_>,
    aggregates: &dyn crate::plan::AggregateClassifier,
    control: &ProductionControl<'_>,
) -> Result<OwnedPLpgSQLVariableBindings, SQLError> {
    control.check()?;
    if control.budget().is_none() {
        let result = parameterize_statement_variables(
            statement,
            resolver,
            conflict,
            &mut |statement, params, names| {
                let plan =
                    UnifiedPlan::lower_borrowed_for_rewrite(&statement, aggregates, control)?;
                let resolutions =
                    resolve_sites(&plan, &params, control.finish(names, None)?, control)?;
                Ok(resolutions
                    .into_uncontrolled()
                    .expect("ordinary variable site callback"))
            },
            aggregates,
        )?;
        return Ok(OwnedPLpgSQLVariableBindings {
            plan: result.plan.copy_for_rewrite(control)?,
            references: control.finish(result.references, None)?,
            parameters: control.finish(result.parameters, None)?,
        });
    }

    let mut sites = ProductionMap::new(*control);
    let mut names = ProductionVec::new(*control);
    let mut params = ProductionVec::new(*control);
    super::visit::Visitor {
        control: *control,
        visit: &mut |expression| {
            let Some(reference) = PLpgSQLVariableReference::from_expression(expression, control)?
            else {
                return Ok(());
            };
            let Some(input) = resolver.resolve_input_with_control(&reference, control)? else {
                return Ok(());
            };
            let name = if matches!(expression, Expr::Param(_)) {
                control.finish(
                    ScalarExpr::Literal(uqa_core::Value::Null),
                    control.empty_reservation(),
                )?
            } else {
                crate::plan::ExpressionPlan::lower_scalar_with_control(expression, control)?
            };
            let (input, memory) = input.into_parts();
            params.push_produced(control.finish(input.parameter, memory)?)?;
            names.push_produced(name)?;
            sites.insert_copy(std::ptr::from_ref(expression) as usize, params.len())?;
            Ok(())
        },
    }
    .statement(statement)?;
    let resolutions = if sites.is_empty() {
        control.finish(Vec::new(), control.empty_reservation())?
    } else {
        let numbered = UnifiedPlan::lower_borrowed_with_parameters(
            statement,
            aggregates,
            control,
            Some(&|expression| {
                sites
                    .get(&(std::ptr::from_ref(expression) as usize))
                    .copied()
            }),
        )?;
        resolve_sites(&numbered, &params.finish()?, names.finish()?, control)?
    };
    // Conflict diagnostics follow the first analysis site, before reading invocation values again.
    let mut keep = ProductionMap::new(*control);
    super::visit::Visitor {
        control: *control,
        visit: &mut |expression| {
            let Some(site) = sites.get(&(std::ptr::from_ref(expression) as usize)) else {
                return Ok(());
            };
            let resolution = resolutions.get(site - 1).copied().ok_or_else(|| {
                SQLError::Internal("procedural site analysis lost a resolution".into())
            })?;
            match (resolution, conflict) {
                (VariableSiteResolution::Output, _)
                | (VariableSiteResolution::Column, VariableConflict::UseColumn) => {
                    keep.insert_copy(std::ptr::from_ref(expression) as usize, true)?;
                }
                (VariableSiteResolution::Column, VariableConflict::Error) => {
                    return Err(super::super::variable_conflicts::ambiguous_variable_error(
                        expression,
                    ))
                }
                _ => {}
            }
            Ok(())
        },
    }
    .statement(statement)?;
    drop(sites);
    drop(resolutions);

    bind_selected_inputs(statement, resolver, aggregates, &keep, control)
}

fn bind_selected_inputs(
    statement: &Statement,
    resolver: &mut dyn VariableResolver,
    aggregates: &dyn crate::plan::AggregateClassifier,
    keep: &ProductionMap<'_, usize, bool>,
    control: &ProductionControl<'_>,
) -> Result<OwnedPLpgSQLVariableBindings, SQLError> {
    let mut selected = ProductionMap::new(*control);
    let mut parameters = ProductionVec::new(*control);
    let mut references = ProductionVec::new(*control);
    let mut collations = ProductionVec::new(*control);
    super::visit::Visitor {
        control: *control,
        visit: &mut |expression| {
            let Some(reference) = PLpgSQLVariableReference::from_expression(expression, control)?
            else {
                return Ok(());
            };
            let Some(input) = resolver.resolve_input_with_control(&reference, control)? else {
                return Ok(());
            };
            if keep
                .get(&(std::ptr::from_ref(expression) as usize))
                .copied()
                .unwrap_or(false)
            {
                return Ok(());
            }
            let (input, memory) = input.into_parts();
            collations.extend_from_slice(&[input.collation_oid])?;
            parameters.push_produced(control.finish(input.parameter, memory)?)?;
            references.push_produced(reference)?;
            selected.insert_copy(std::ptr::from_ref(expression) as usize, parameters.len())?;
            Ok(())
        },
    }
    .statement(statement)?;
    let mut plan = UnifiedPlan::lower_borrowed_with_parameters(
        statement,
        aggregates,
        control,
        Some(&|expression| {
            selected
                .get(&(std::ptr::from_ref(expression) as usize))
                .copied()
        }),
    )?;
    let parameters = parameters.finish()?;
    plan = plan.transform_scalar_nodes(control, &mut |_, expression| {
        let ScalarExpr::Param(number) = &*expression else {
            return Ok(expression);
        };
        let Some(index) = number.checked_sub(1) else {
            return Ok(expression);
        };
        let Some(oid) = collations.get(index).copied().flatten() else {
            return Ok(expression);
        };
        let Some(ty) = parameters[index]
            .declared_scalar_type()
            .filter(|ty| crate::schema::composites::type_is_collatable(ty))
        else {
            return Ok(expression);
        };
        Ok(crate::plan::OwnedScalarExpr::collated(
            expression,
            ty.clone_with_control(control)?,
            crate::ir::ResultCollation::new(
                crate::catalog::collations::derivation::Derivation::implicit(i64::from(oid)),
                oid,
            ),
            control,
        )?)
    })?;
    Ok(OwnedPLpgSQLVariableBindings {
        plan,
        references: references.finish()?,
        parameters,
    })
}
