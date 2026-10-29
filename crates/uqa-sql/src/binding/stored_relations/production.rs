//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical row-relation references preserve namespace and kind diagnostics while admitting each selected name.

use super::{RelationLookupMode, RelationResolution, StoredRelationCatalog};
use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub(crate) fn canonical_catalog_relation_with_control(
    catalog: &dyn StoredRelationCatalog,
    reference: &str,
    lookup_mode: RelationLookupMode,
    loaded_catalog: bool,
    context: &str,
    control: &ProductionControl<'_>,
) -> Result<(Produced<String>, bool), SQLError> {
    control.check()?;
    if lookup_mode == RelationLookupMode::Dynamic {
        if let Some(canonical) =
            catalog.resolve_age_label_relation_name_with_control(reference, control)?
        {
            validate(&canonical, control)?;
            return Ok((canonical, true));
        }
    }
    let resolution = match (lookup_mode, loaded_catalog) {
        (RelationLookupMode::Dynamic, true) => {
            catalog.resolve_loaded_visible_relation_kind_with_control(reference, control)?
        }
        (RelationLookupMode::Dynamic, false) => {
            catalog.resolve_visible_relation_kind_with_control(reference, control)?
        }
        (RelationLookupMode::Bound, _) => {
            catalog.resolve_bound_relation_kind_with_control(reference, control)?
        }
    };
    let (resolution, memory) = resolution.into_parts();
    let canonical = match resolution {
        RelationResolution::Found(
            name,
            "table" | "view" | "materialized view" | "foreign table" | "sequence",
        ) => control.finish(name, memory)?,
        RelationResolution::Found(name, "composite type") => {
            return Err(super::query::composite_relation_error(&name))
        }
        RelationResolution::Found(name, kind) => {
            return Err(SQLError::Routine {
                sqlstate: control.copy_text("42809")?.into_parts().0,
                message: control
                    .format(format_args!(
                        "{context} source \"{name}\" is a {kind}, not a row relation"
                    ))?
                    .into_parts()
                    .0,
            })
        }
        RelationResolution::MissingSchema(name) => {
            return Err(SQLError::Routine {
                sqlstate: control.copy_text("3F000")?.into_parts().0,
                message: control
                    .format(format_args!("schema \"{name}\" does not exist"))?
                    .into_parts()
                    .0,
            })
        }
        RelationResolution::MissingRelation => {
            return Err(SQLError::UnknownTable(
                control.copy_text(reference)?.into_parts().0,
            ))
        }
    };
    validate(&canonical, control)?;
    Ok((canonical, false))
}

fn validate(canonical: &str, control: &ProductionControl<'_>) -> Result<(), SQLError> {
    RelationIdentity::try_borrowed_reference(canonical)
        .map(|_| ())
        .map_err(|error| {
            control
                .format(format_args!(
                    "decode bound rule source `{canonical}`: {error}"
                ))
                .map_or_else(Into::into, |message| {
                    SQLError::Internal(message.into_parts().0)
                })
        })
}
