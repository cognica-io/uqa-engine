//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation OID input and the soft-error boundary of `to_regcollation`.

use super::{parse_dash_or_oid, CatalogContext, OutputVisibility, SQLError};
use crate::catalog::security::schema::SchemaAclPrivilege;
use uqa_sql::catalog::collations::{builtin_collation_name, builtin_collation_oid};

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
    let names = uqa_sql::parse_regobject_name(input)
        .ok_or_else(|| input_error("42602", "invalid name syntax".into()))?;
    let (schema, local) = match names.as_slice() {
        [local] => (None, local),
        [schema, local] => (Some(schema.as_str()), local),
        [database, schema, local] if database == "uqa" => (Some(schema.as_str()), local),
        [_, _, _] => return Err(super::cross_database_reference(&names.join("."))),
        _ => {
            return Err(input_error(
                "42601",
                format!(
                    "improper qualified name (too many dotted names): {}",
                    names.join(".")
                ),
            ))
        }
    };
    let visible = match schema {
        Some(schema) => {
            if context.schema_security_for_privilege(schema).is_some() {
                context.require_schema_privilege(
                    schema,
                    &context.current_role(),
                    SchemaAclPrivilege::Usage,
                )?;
            }
            schema == "pg_catalog"
        }
        None => context
            .current_schema_names(true)?
            .iter()
            .any(|schema| schema == "pg_catalog"),
    };
    if let Some(oid) = visible.then(|| builtin_collation_oid(local)).flatten() {
        return Ok(Some(oid));
    }
    Err(input_error(
        "42704",
        format!(
            "collation \"{}\" for encoding \"UTF8\" does not exist",
            names.join(".")
        ),
    ))
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
