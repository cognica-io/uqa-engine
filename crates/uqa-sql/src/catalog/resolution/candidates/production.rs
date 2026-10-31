//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Candidate identities are produced from borrowed namespace inputs in legacy lookup order.

use super::RelationCandidateState;
use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity, RelationName, RelationReference,
};

pub fn relation_lookup_candidates_with_control(
    state: &dyn RelationCandidateState,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<RelationIdentity>>, SQLError> {
    control.check()?;
    let reference = RelationIdentity::try_borrowed_reference(name).map_err(|error| {
        control
            .format(format_args!("{error}"))
            .map(|message| SQLError::Internal(message.into_parts().0))
            .unwrap_or_else(Into::into)
    })?;
    let mut output = ProductionVec::new(*control);
    if let Some(schema) = reference.schema {
        if schema == RelationName::decoded("pg_temp") {
            let temporary = state.temporary_schema_name_with_control(control)?;
            let reference = RelationReference {
                schema: None,
                ..reference
            };
            output.push_produced(
                reference.identity_with_control(RelationName::decoded(&temporary), control)?,
            )?;
        } else {
            output.push_produced(reference.identity_with_control(schema, control)?)?;
        }
        return Ok(output.finish()?);
    }
    let temporary = state.temporary_schema_name_with_control(control)?;
    state.with_search_path_with_control(control, &mut |path| {
        let mut has_temporary = false;
        let mut has_catalog = false;
        for index in 0..path.len() {
            control.check()?;
            let schema = path.schema(index);
            has_temporary |= schema == "pg_temp" || schema == temporary.as_str();
            has_catalog |= schema == "pg_catalog";
        }
        let mut append = |schema: &str| -> Result<(), SQLError> {
            output.push_produced(
                reference.identity_with_control(RelationName::decoded(schema), control)?,
            )?;
            Ok(())
        };
        if !has_temporary {
            append(&temporary)?;
        }
        if !has_catalog {
            append("pg_catalog")?;
        }
        for index in 0..path.len() {
            control.check()?;
            let schema = path.schema(index);
            append(if schema == "pg_temp" {
                &temporary
            } else {
                schema
            })?;
        }
        Ok(())
    })?;
    Ok(output.finish()?)
}
