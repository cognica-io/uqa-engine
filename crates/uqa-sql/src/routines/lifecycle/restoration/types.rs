//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Signature identity replacement belongs to the native declaration, while its catalog selection belongs to Execution.

use crate::ast::{CreateFunction, FunctionReturns, TypeIdentityInputError, UserTypeIdentity};
use std::future::Future;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
};

pub trait RoutineTypeIdentityInput: Send {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Resolve a historical type spelling against the selected catalog and namespace. Built-ins and unresolvable names have no replacement; existing serialized identities never reach this callback.
    fn resolve_user_type_identity(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = Result<Option<UserTypeIdentity>, Self::Error>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum RoutineTypeBindingError<E: std::error::Error + Send + Sync + 'static> {
    #[error(transparent)]
    Input(#[from] TypeIdentityInputError),
    #[error(transparent)]
    Lookup(E),
}

impl<E: std::error::Error + Send + Sync + 'static> From<uqa_core::image_io::ImageIoError>
    for RoutineTypeBindingError<E>
{
    fn from(error: uqa_core::image_io::ImageIoError) -> Self {
        Self::Input(error.into())
    }
}

impl CreateFunction {
    /// Catalog restoration binds every parameter declaration, including OUT/TABLE, and a scalar or SETOF return declaration in original order. A TABLE result's separate declaration keeps the original restoration behavior.
    pub fn signature_type_names_mut(&mut self) -> impl Iterator<Item = &mut String> {
        let returned = match &mut self.returns {
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
                Some(type_name)
            }
            _ => None,
        };
        self.params
            .iter_mut()
            .map(|parameter| &mut parameter.type_name)
            .chain(returned)
    }
}

/// Consume a prepared native declaration while replacing only names whose selected catalog type has a stable identity. Failure or abandonment retires the complete unpublished declaration under its original allowance.
pub async fn identify_routine_signature_types_from_input<R: RoutineTypeIdentityInput>(
    definition: Produced<CreateFunction>,
    resolver: &mut R,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Produced<CreateFunction>, RoutineTypeBindingError<R::Error>> {
    control.check().map_err(TypeIdentityInputError::from)?;
    assert!(
        definition.is_retained_by(control.budget().expect("admitted routine restoration")),
        "routine signature replacement keeps its original allowance"
    );
    let (mut definition, mut memory) = definition.into_parts();
    for name in definition.signature_type_names_mut() {
        control.check().map_err(TypeIdentityInputError::from)?;
        input
            .checkpoint()
            .await
            .map_err(TypeIdentityInputError::from)?;
        if UserTypeIdentity::parse_from_input(name, control, input)
            .await?
            .is_some()
        {
            continue;
        }
        let Some(identity) = resolver
            .resolve_user_type_identity(name, control, input)
            .await
            .map_err(RoutineTypeBindingError::Lookup)?
        else {
            continue;
        };
        let replacement = identity.render_from_input(control, input).await?;
        let (replacement, replacement_memory) = replacement.into_parts();
        let previous = std::mem::replace(name, replacement);
        let bytes = previous.capacity();
        drop(previous);
        drop(memory.as_mut().expect("admitted declaration").split(bytes));
        memory = control.combine(memory, replacement_memory);
    }
    control
        .finish(definition, memory)
        .map_err(TypeIdentityInputError::from)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests;
