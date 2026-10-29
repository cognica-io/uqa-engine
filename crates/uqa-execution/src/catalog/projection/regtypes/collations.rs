//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation namespace selection and the OID-input and soft-error boundaries of `regcollation`.

use super::{parse_dash_or_oid, CatalogContext, OutputVisibility, SQLError};
use crate::catalog::security::schema::SchemaAclPrivilege;
use uqa_sql::catalog::collations::{builtin_collation_name, builtin_collation_oid};
mod production;
pub use production::resolve_expression_collation_oid_with_control;

fn input_error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

/// `regcollationin`: numeric OIDs pass through; names identify a collation available for UTF8.
pub fn resolve_regcollation_oid(
    context: &CatalogContext<'_>,
    input: &str,
) -> Result<Option<i64>, SQLError> {
    if let Some(oid) = parse_dash_or_oid(input)? {
        return Ok(Some(oid));
    }
    resolve_named_collation(context, input, true).map(Some)
}

/// A COLLATE name must identify an existing namespace and collation; numeric OID input and soft missing-schema lookup belong only to `regcollation`.
pub fn resolve_expression_collation_oid(
    context: &CatalogContext<'_>,
    input: &str,
) -> Result<u32, SQLError> {
    resolve_expression_collation_oid_with_control(
        context,
        input,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub(super) fn resolve_regcollation_name_with_control(
    context: &CatalogContext<'_>,
    input: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<i64, SQLError> {
    production::resolve_named_collation(context, input, true, control)
}

fn resolve_named_collation(
    context: &CatalogContext<'_>,
    input: &str,
    missing_schema_ok: bool,
) -> Result<i64, SQLError> {
    production::resolve_named_collation(
        context,
        input,
        missing_schema_ok,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub(super) fn lookup_regcollation_oid(
    context: &CatalogContext<'_>,
    input: &str,
) -> Result<Option<i64>, SQLError> {
    match resolve_regcollation_oid(context, input) {
        Err(SQLError::Routine { sqlstate, .. })
            if matches!(sqlstate.as_str(), "22P02" | "22003" | "42602" | "42704") =>
        {
            Ok(None)
        }
        result => result,
    }
}

pub(super) fn format_regcollation(visibility: &OutputVisibility, oid: i64) -> Option<String> {
    let name = builtin_collation_name(oid)?;
    Some(
        if visibility
            .schemas
            .iter()
            .any(|schema| schema == "pg_catalog")
        {
            uqa_sql::expr::quote_ident(name)
        } else {
            super::qualified_name("pg_catalog", name)
        },
    )
}

/// Reconstruct a selected collation using only namespace visibility, without building unrelated type metadata.
pub(crate) fn format_expression_collation(
    catalog: &crate::catalog::CatalogReadView,
    resolution: &crate::catalog::RelationNameResolution,
    oid: u32,
) -> Option<String> {
    format_regcollation(
        &OutputVisibility::from_resolution(catalog, resolution),
        i64::from(oid),
    )
}

#[cfg(test)]
mod namespaces_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collation_output_matches_independent_postgresql_names() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/parity/pg18/regcollation_oracle.expected.json"
        )))
        .unwrap();
        let case = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == "known_oids")
            .unwrap();
        let visibility = OutputVisibility {
            schemas: vec!["pg_catalog".into(), "public".into()],
        };
        for (oid, expected) in [950, 951, 100, 811, 962, 963, 6411]
            .into_iter()
            .zip(case["results"][0]["rows"][0].as_array().unwrap())
        {
            assert_eq!(
                format_regcollation(&visibility, oid).as_deref(),
                expected.as_str()
            );
        }
        assert_eq!(format_regcollation(&visibility, 0), None);
        assert_eq!(format_regcollation(&visibility, 123_456), None);
    }
}
