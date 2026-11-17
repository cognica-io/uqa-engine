//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored query names share catalog selection and preserve the destination's original lease.

use super::{composite_relation_error, StoredQueryBindingContext};
use crate::{
    catalog::resolution::{RelationLookupMode, RelationResolution},
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity, RelationName,
};

impl<R: super::StoredRelationCatalog + ?Sized, S: ?Sized> StoredQueryBindingContext<'_, R, S> {
    /// Bind the complete relation pass before sequence binding. Existing scoped traversal owns CTE visibility and mutation children; canonical names replace only their original admitted source fields.
    pub fn bind_query_relation_names_with_control(
        &self,
        query: crate::plan::MutableQueryPlan,
        context: &str,
        reject_transition_relations: bool,
        loaded_catalog: bool,
        control: &ProductionControl<'_>,
    ) -> Result<(crate::plan::MutableQueryPlan, bool), SQLError> {
        let mut uses_temporary = false;
        let query = crate::binding::view_dependencies::bind_query_relation_targets_owned(
            query,
            &|_| false,
            true,
            control,
            &mut |reference, _, control| {
                let (name, temporary) = self.canonical_relation_with_control(
                    reference,
                    context,
                    reject_transition_relations,
                    loaded_catalog,
                    control,
                )?;
                uses_temporary |= temporary;
                Ok(name)
            },
        )?;
        Ok((query, uses_temporary))
    }

    /// Select one stored query source and retain the canonical name under the destination allowance. Query traversal owns CTE visibility; this boundary owns catalog mode, transition names, source kind and temporary-relation observation.
    pub fn canonical_relation_with_control(
        &self,
        reference: &str,
        context: &str,
        reject_transition_relations: bool,
        loaded_catalog: bool,
        control: &ProductionControl<'_>,
    ) -> Result<(Produced<String>, bool), SQLError> {
        self.relation_names().canonical(
            reference,
            context,
            reject_transition_relations,
            loaded_catalog,
            control,
        )
    }

    fn relation_names(&self) -> Names<'_, R> {
        Names {
            relations: self.relations,
            lookup_mode: self.lookup_mode,
            temporary_schema: self.temporary_schema,
            transition_relations: self.transition_relations,
        }
    }
}

struct Names<'a, R: ?Sized> {
    relations: &'a R,
    lookup_mode: RelationLookupMode,
    temporary_schema: &'a str,
    transition_relations: &'a std::collections::BTreeSet<String>,
}

impl<R: super::StoredRelationCatalog + ?Sized> Names<'_, R> {
    fn canonical(
        &self,
        reference: &str,
        context: &str,
        reject_transition_relations: bool,
        loaded_catalog: bool,
        control: &ProductionControl<'_>,
    ) -> Result<(Produced<String>, bool), SQLError> {
        control.check()?;
        if self.lookup_mode == RelationLookupMode::Dynamic {
            let canonical = if control.budget().is_some() {
                self.relations
                    .resolve_age_label_relation_name_with_control(reference, control)?
            } else {
                self.relations
                    .resolve_age_label_relation_name(reference)?
                    .map(|name| control.finish(name, None))
                    .transpose()?
            };
            if let Some(canonical) = canonical {
                return Ok((canonical, false));
            }
        }
        let transition = match RelationIdentity::borrowed_reference(reference) {
            Some(reference)
                if reference.schema.is_none() && !self.transition_relations.is_empty() =>
            {
                let name = control.format(format_args!("{}", reference.name))?;
                self.transition_relations.contains(name.as_str())
            }
            _ => false,
        };
        if transition {
            if reject_transition_relations {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "transition tables cannot be referenced in a view definition".into(),
                });
            }
            return Ok((control.copy_text(reference)?, false));
        }
        let resolution = super::super::production::resolve_catalog_relation_kind_with_control(
            self.relations,
            reference,
            self.lookup_mode,
            loaded_catalog,
            control,
        )?;
        let (resolution, memory) = resolution.into_parts();
        match resolution {
            RelationResolution::Found(
                canonical,
                "table" | "view" | "materialized view" | "foreign table",
            ) => {
                let temporary =
                    RelationIdentity::borrowed_reference(&canonical).is_some_and(|reference| {
                        reference.schema.unwrap_or(RelationName::decoded("public"))
                            == RelationName::decoded(self.temporary_schema)
                    });
                Ok((control.finish(canonical, memory)?, temporary))
            }
            RelationResolution::Found(canonical, "composite type") => {
                Err(composite_relation_error(&canonical))
            }
            RelationResolution::Found(canonical, kind) => Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!(
                    "{context} source \"{canonical}\" is a {kind}, not a row relation"
                ),
            }),
            RelationResolution::MissingSchema(_) | RelationResolution::MissingRelation => {
                Err(SQLError::UnknownTable(reference.to_string()))
            }
        }
    }
}

impl<R: super::StoredRelationCatalog + Sync + ?Sized, S: ?Sized>
    StoredQueryBindingContext<'_, R, S>
{
    /// Bind retained query and mutation relation names through the shared scoped traversal under the plan's original destination allowance. Sequence and routine passes remain separate and run after the complete relation pass. Catalog callbacks retain their existing atomic interface and original canonical-name owners.
    pub fn bind_query_relation_names_from_input<'a, 'i: 'a>(
        &'a self,
        plan: crate::plan::RetainedExecutablePlan,
        context: &'a str,
        reject_transition_relations: bool,
        loaded_catalog: bool,
        control: &'a ProductionControl<'a>,
        input: &'a mut uqa_core::image_io::AsyncImageInput<'i>,
    ) -> impl std::future::Future<
        Output = Result<(crate::plan::RetainedExecutablePlan, bool), SQLError>,
    >
           + Send
           + 'a
           + use<'a, 'i, R, S> {
        let names = self.relation_names();
        async move {
            plan.bind_relation_names_from_input(
                &|reference, _, control| {
                    names.canonical(
                        reference,
                        context,
                        reject_transition_relations,
                        loaded_catalog,
                        control,
                    )
                },
                control,
                input,
            )
            .await
        }
    }
}
