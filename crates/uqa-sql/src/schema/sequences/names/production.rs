//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored sequence names are selected from borrowed registry keys before producing canonical output.

use super::StoredSequenceRegistry;
use crate::SQLError;
use std::fmt::Write;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    RelationIdentity,
};

pub fn resolve_stored_sequence_reference_with_control(
    catalog: &dyn StoredSequenceRegistry,
    reference: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    let parsed = match RelationIdentity::try_borrowed_reference(reference) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Err(SQLError::Internal(
                control
                    .format(format_args!(
                        "invalid persisted sequence reference `{reference}`: {error}"
                    ))?
                    .into_parts()
                    .0,
            ))
        }
    };
    let mut candidates = ProductionVec::new(*control);
    catalog.visit_sequence_names_with_control(control, &mut |candidate| {
        if parsed.name == uqa_core::RelationName::decoded(&candidate.name)
            && parsed
                .schema
                .is_none_or(|schema| schema == uqa_core::RelationName::decoded(&candidate.schema))
        {
            candidates.push_produced(candidate.qualified_name_with_control(control)?)?;
        }
        Ok(())
    })?;
    let candidates = candidates.finish()?;
    if candidates.len() == 1 {
        return Ok(control.copy_text(&candidates[0])?);
    }
    if candidates.is_empty() {
        return Err(SQLError::Internal(
            control
                .format(format_args!(
                    "dangling persisted sequence reference `{reference}`"
                ))?
                .into_parts()
                .0,
        ));
    }
    let mut message = ProductionString::new(*control);
    let formatted = write!(
        &mut message,
        "ambiguous persisted sequence reference `{reference}` matches "
    );
    for (position, candidate) in candidates.iter().enumerate() {
        if position != 0 {
            let _ = message.write_str(", ");
        }
        let _ = message.write_str(candidate);
    }
    let message = message.finish()?;
    debug_assert!(formatted.is_ok());
    Err(SQLError::Internal(message.into_parts().0))
}
