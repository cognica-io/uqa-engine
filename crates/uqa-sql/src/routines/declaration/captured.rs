//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured declarations preserve creation rules without executing defaults or compiling source bodies.

use super::{parameters::ParameterLayout, validation, RoutineTypeCatalog};
use crate::{
    ast::{ColumnType, CreateFunction},
    routines::{attributes, registration},
    SQLError,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
};

pub fn validate_captured_declaration(
    definition: &CreateFunction,
    types: &dyn RoutineTypeCatalog,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut check = CapturedDeclarationCheck::default();
    loop {
        match crate::catalog::work::inline(check.read(definition, control, &mut ()))? {
            CapturedDeclarationStep::Complete => return Ok(()),
            CapturedDeclarationStep::Variadic(name, next) => {
                check = next.resolve(catalog_variadic_array(types, &name, control)?)?;
            }
        }
    }
}

/// The argument position and layout already checked at the exact point where a declaration needs a live catalog lookup.
#[derive(Default)]
pub struct CapturedDeclarationCheck {
    position: usize,
    layout: ParameterLayout,
    after_variadic: bool,
}

pub enum CapturedDeclarationStep {
    Complete,
    Variadic(Produced<String>, CapturedVariadicArgument),
}

/// A suspended declaration may resume only after its original VARIADIC type has been accepted.
pub struct CapturedVariadicArgument(CapturedDeclarationCheck);

impl CapturedVariadicArgument {
    pub fn resolve(self, array: bool) -> Result<CapturedDeclarationCheck, SQLError> {
        require_variadic_array(array)?;
        Ok(self.0)
    }
}

impl CapturedDeclarationCheck {
    pub async fn read_from_input(
        self,
        definition: &CreateFunction,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<CapturedDeclarationStep, SQLError> {
        self.read(definition, control, input).await
    }

    async fn read<W: validation::Work>(
        mut self,
        definition: &CreateFunction,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<CapturedDeclarationStep, SQLError> {
        control.check()?;
        if !self.after_variadic {
            attributes::validate_cost(definition.cost)?;
            attributes::validate_rows(definition.rows)?;
            if let Some(support) = definition.support.as_deref() {
                registration::validate_routine_support_identity_with_work(support, control, work)
                    .await?;
            }
            attributes::validate_routine_language_with_work(definition, control, work).await?;
            attributes::validate_body_form(definition, &definition.attribute_clauses)?;
        }
        while let Some(parameter) = definition.params.get(self.position) {
            work.checkpoint().await?;
            control.check()?;
            if !self.after_variadic
                && self
                    .layout
                    .begin(parameter.mode, definition.is_procedure, control)?
            {
                let name = work.canonical(&parameter.type_name, control).await?;
                if !known_variadic_array(&name) {
                    self.after_variadic = true;
                    return Ok(CapturedDeclarationStep::Variadic(
                        name,
                        CapturedVariadicArgument(self),
                    ));
                }
            }
            self.after_variadic = false;
            self.layout
                .finish(
                    &definition.params[..self.position],
                    parameter,
                    definition.is_procedure,
                    control,
                    work,
                )
                .await?;
            self.position += 1;
        }
        attributes::validate_rows_applicability(definition.rows, definition.returns_set())?;
        validation::validate(definition, control, work).await?;
        Ok(CapturedDeclarationStep::Complete)
    }
}

pub(super) fn require_variadic_array(array: bool) -> Result<(), SQLError> {
    if array {
        Ok(())
    } else {
        Err(super::routine_definition_error(
            "VARIADIC parameter must be an array",
        ))
    }
}

pub(in crate::routines) fn validate_language(definition: &CreateFunction) -> Result<(), SQLError> {
    attributes::validate_routine_language(definition)?;
    attributes::validate_body_form(definition, &definition.attribute_clauses)
}

pub(super) fn variadic_array(
    types: &dyn RoutineTypeCatalog,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let canonical =
        crate::type_resolution::canonical_routine_type_name_with_control(name, control)?;
    if known_variadic_array(&canonical) {
        return Ok(true);
    }
    catalog_variadic_array(types, &canonical, control)
}

fn known_variadic_array(canonical: &str) -> bool {
    canonical.ends_with("[]")
        || matches!(
            canonical,
            "any" | "anyarray" | "anycompatiblearray" | "int2vector" | "oidvector"
        )
}

/// Resolve a remaining canonical VARIADIC type only where argument validation requests it.
pub fn catalog_variadic_array(
    types: &dyn RoutineTypeCatalog,
    canonical: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let Some(ty) = types.resolve_catalog_column_type_with_control(canonical, control)? else {
        return Ok(false);
    };
    Ok(matches!(&*ty, ColumnType::Array(_) | ColumnType::AnyArray))
}

#[cfg(test)]
mod tests;
