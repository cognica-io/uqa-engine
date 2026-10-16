//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate range claims against borrowed catalog identities without materializing catalog rows.

use crate::catalog::{projection, CatalogReadView};
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

/// Structural validation already checks the range registry itself. Complete restoration calls this after other type and relation owners have restored their identities. Empty registries perform no further lookup.
pub fn validate_catalog(catalog: &CatalogReadView) -> Result<(), SQLError> {
    for range in catalog.ranges() {
        if range
            .type_oids()
            .into_iter()
            .any(|oid| projection::non_range_type_oid_in_use(catalog, i64::from(oid)))
        {
            return Err(collision());
        }
        for (identity, array) in [
            (&range.identity, &range.array_name),
            (&range.multirange.identity, &range.multirange.array_name),
        ] {
            for name in [&identity.name, array] {
                let identity = RelationIdentity::new(&identity.schema, name);
                if projection::named_type_exists(
                    catalog.domains(),
                    catalog.enums(),
                    catalog.composites(),
                    catalog.shells(),
                    std::iter::empty(),
                    &identity,
                ) || crate::catalog::composite_type::relations::by_name(
                    catalog,
                    &identity.schema,
                    &identity.name,
                )
                .is_some()
                {
                    return Err(collision());
                }
            }
        }
        let endpoint = uqa_sql::catalog::type_metadata::pg_type_oid(&range.subtype);
        if !projection::type_oid_in_use(catalog, endpoint)
            || range.type_oids().map(i64::from).contains(&endpoint)
        {
            return Err(SQLError::Internal(format!(
                "range {} has a missing or self-referencing subtype",
                range.identity.qualified_name()
            )));
        }
    }
    Ok(())
}

fn collision() -> SQLError {
    SQLError::Internal("range type claims collide with another catalog type".into())
}
