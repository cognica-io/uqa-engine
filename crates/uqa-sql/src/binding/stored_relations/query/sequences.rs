//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored query sequence selection retains ordered namespace candidates and typed production failures.

use super::{Produced, ProductionControl, RelationIdentity, SQLError, StoredQueryBindingContext};
use uqa_core::Value;

pub fn select_query_sequence_with_control(
    candidates: &[RelationIdentity],
    mut contains: impl FnMut(&RelationIdentity) -> bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>, SQLError> {
    control.check()?;
    for candidate in candidates {
        control.check()?;
        if contains(candidate) {
            return Ok(Some(candidate.qualified_name_with_control(control)?));
        }
    }
    control.check()?;
    Ok(None)
}

pub(super) fn resolve_loaded_query_sequence_with_control(
    reference: &str,
    candidates: &[RelationIdentity],
    contains: impl FnMut(&RelationIdentity) -> bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    select_query_sequence_with_control(candidates, contains, control)?
        .ok_or_else(|| SQLError::Unsupported(format!("Sequence `{reference}` does not exist")))
}

pub fn resolve_query_sequence_with_control(
    state: &dyn crate::catalog::resolution::candidates::RelationCandidateState,
    reference: &str,
    contains: impl FnMut(&RelationIdentity) -> bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let candidates =
        crate::catalog::resolution::candidates::relation_lookup_candidates_with_control(
            state, reference, control,
        )
        .map_err(|error| match error {
            SQLError::Internal(message) => SQLError::Unsupported(message),
            error => error,
        })?;
    resolve_loaded_query_sequence_with_control(reference, &candidates, contains, control)
}

pub(super) fn external_name(
    resolve: impl FnOnce() -> Result<String, String>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    let name = resolve().map_err(SQLError::Unsupported)?;
    let (value, memory) = control
        .retain_external_value(Value::Str(name))?
        .into_parts();
    let Value::Str(name) = value else {
        unreachable!("sequence output name")
    };
    Ok(control.finish(name, memory)?)
}

impl<R: ?Sized, S: super::StoredQuerySequences + ?Sized> StoredQueryBindingContext<'_, R, S> {
    pub fn canonical_sequence_with_control(
        &self,
        reference: &str,
        context: &str,
        loaded_catalog: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        control.check()?;
        let name = if control.budget().is_some() {
            if loaded_catalog {
                self.sequences
                    .loaded_query_sequence_with_control(reference, control)
            } else {
                self.sequences
                    .query_sequence_with_control(reference, control)
            }
        } else {
            let name = if loaded_catalog {
                self.sequences.loaded_query_sequence(reference)
            } else {
                self.sequences.query_sequence(reference)
            };
            name.map_err(SQLError::Unsupported)
                .and_then(|name| Ok(control.finish(name, None)?))
        }
        .map_err(|error| match error {
            SQLError::Unsupported(message) => SQLError::Unsupported(format!(
                "{context} sequence reference `{reference}`: {message}"
            )),
            error => error,
        })?;
        control.check()?;
        Ok(name)
    }
}

impl<
        R: super::StoredRelationCatalog + Sync + ?Sized,
        S: super::StoredQuerySequences + Sync + ?Sized,
    > StoredQueryBindingContext<'_, R, S>
{
    /// Complete relation binding before observing any sequence name. Both passes keep the retained statement's original constructor ownership; routine analysis remains the next owning boundary.
    pub async fn bind_query_names_from_input(
        &self,
        plan: crate::plan::RetainedExecutablePlan,
        context: &str,
        reject_transition_relations: bool,
        loaded_catalog: bool,
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<(crate::plan::RetainedExecutablePlan, bool), SQLError> {
        let (plan, temporary) = self
            .bind_query_relation_names_from_input(
                plan,
                context,
                reject_transition_relations,
                loaded_catalog,
                control,
                input,
            )
            .await?;
        let plan = plan
            .bind_sequence_references_from_input(
                &|reference, control| {
                    self.canonical_sequence_with_control(
                        reference,
                        context,
                        loaded_catalog,
                        control,
                    )
                },
                control,
                input,
            )
            .await?;
        Ok((plan, temporary))
    }
}

#[cfg(test)]
mod tests;
