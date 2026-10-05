//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View DROP authorization, dependency diagnostics and temporary dependency layers.
use super::StoredView;
use crate::{
    catalog::security::view_ownership::{self, ViewOwnershipContext},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;

pub fn ensure_view_drop_authorities(
    ownership: ViewOwnershipContext<'_>,
    names: &[String],
    views: &BTreeMap<RelationIdentity, StoredView>,
) -> Result<(), SQLError> {
    for name in names {
        let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
            SQLError::Internal(format!("resolve DROP VIEW target `{name}`: {error}"))
        })?;
        let view = views.get(&relation).ok_or_else(|| {
            SQLError::Internal(format!("view `{name}` disappeared before owner check"))
        })?;
        view_ownership::ensure_view_drop_authority(ownership, name, view)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
