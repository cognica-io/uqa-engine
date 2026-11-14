//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained role subjects preserve named lookup and renamed-incarnation fallback.

use super::{RoleBinding, RoleDefinition, RoleDefinitions, RoleReference};
use crate::{catalog::work::Work, SQLError};
use std::cmp::Ordering;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MapKeyInput, ProductionControl},
};

impl RoleReference {
    pub async fn role_definition_from_input<'a>(
        &self,
        roles: &'a RoleDefinitions,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<&'a RoleDefinition>, SQLError> {
        control.check()?;
        let selected = match self {
            Self::Named(name) => named(name, roles, control, input).await,
            Self::Bound(binding) => bound(binding, roles, control, input).await,
        }?;
        control.check()?;
        Ok(selected)
    }
}

#[cfg(test)]
mod tests;

async fn named<'a>(
    name: &str,
    roles: &'a RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<Option<&'a RoleDefinition>, SQLError> {
    roles
        .get_from(
            name,
            &mut Comparison {
                control: *control,
                work,
            },
        )
        .await
}

pub(super) async fn bound<'a>(
    binding: &RoleBinding,
    roles: &'a RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<Option<&'a RoleDefinition>, SQLError> {
    control.check()?;
    if let Some(role) = named(&binding.name, roles, control, work)
        .await?
        .filter(|role| binding.matches_definition(role))
    {
        return Ok(Some(role));
    }
    for role in roles.values() {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        if binding.matches_definition(role) {
            return Ok(Some(role));
        }
    }
    Ok(None)
}

struct Comparison<'a, 'b, W> {
    control: ProductionControl<'a>,
    work: &'b mut W,
}
impl<W: Work + Send> MapKeyInput<str, String> for Comparison<'_, '_, W> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        self.control.check()?;
        self.work.checkpoint().await?;
        self.control.check()?;
        Ok(())
    }
    async fn compare(&mut self, query: &str, stored: &String) -> Result<Ordering, Self::Error> {
        for (left, right) in query
            .as_bytes()
            .chunks(4096)
            .zip(stored.as_bytes().chunks(4096))
        {
            self.checkpoint().await?;
            let order = left.cmp(right);
            if order.is_ne() {
                return Ok(order);
            }
        }
        Ok(query.len().cmp(&stored.len()))
    }
}
