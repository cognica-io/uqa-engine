//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP relation target binding and label protection.
use crate::{ast::DropKind, catalog::resolution::RelationResolution, SQLError};

pub trait RelationDropCatalog {
    fn resolve_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError>;
    fn resolve_age_label_relation_name(&self, name: &str) -> Result<Option<String>, SQLError>;
}
pub fn validate_drop_table_label_target(
    catalog: &dyn RelationDropCatalog,
    name: &str,
) -> Result<(), SQLError> {
    if let Some(canonical) = catalog.resolve_age_label_relation_name(name)? {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
                SQLError::Internal(format!(
                    "resolve AGE label relation `{canonical}` for DROP TABLE: {error}"
                ))
            })?;
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "table \"{}\" is for label \"{}\"",
                relation.name, relation.name
            ),
        });
    }
    Ok(())
}

pub fn drop_relation_kind(kind: DropKind) -> &'static str {
    match kind {
        DropKind::Table => "table",
        DropKind::ForeignTable => "foreign table",
        DropKind::ForeignServer => "server",
        DropKind::ForeignWrapper => "foreign-data wrapper",
        DropKind::View => "view",
        DropKind::MaterializedView => "materialized view",
        DropKind::Sequence => "sequence",
        DropKind::Index => "index",
        DropKind::Schema => "schema",
        DropKind::Domain => "domain",
        DropKind::Type => "type",
    }
}

/// `DropErrorMsgWrongType`: a DROP names a relation of another kind, and the hint names the command that drops the kind it is.
pub fn wrong_drop_kind_error(local: &str, expected: &str, found: &str) -> SQLError {
    let article = if expected == "index" { "an" } else { "a" };
    let hint = match found {
        "table" => Some("Use DROP TABLE to remove a table."),
        "sequence" => Some("Use DROP SEQUENCE to remove a sequence."),
        "view" => Some("Use DROP VIEW to remove a view."),
        "materialized view" => Some("Use DROP MATERIALIZED VIEW to remove a materialized view."),
        "index" => Some("Use DROP INDEX to remove an index."),
        "foreign table" => Some("Use DROP FOREIGN TABLE to remove a foreign table."),
        "type" | "composite type" => Some("Use DROP TYPE to remove a type."),
        _ => None,
    };
    SQLError::Diagnostic {
        sqlstate: "42809".into(),
        message: format!("\"{local}\" is not {article} {expected}"),
        detail: None,
        hint: hint.map(str::to_string),
    }
}

/// Resolve one requested name so execution can repeat the same policy after a lock wait.
pub fn bind_relation_drop_target(
    catalog: &dyn RelationDropCatalog,
    name: &str,
    kind: DropKind,
    if_exists: bool,
    notice: &mut dyn FnMut(&str),
) -> Result<Option<String>, SQLError> {
    if kind == DropKind::Table {
        validate_drop_table_label_target(catalog, name)?;
    }
    let expected = drop_relation_kind(kind);
    let (_, local) =
        uqa_core::RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
    match catalog.resolve_relation_kind(name)? {
        RelationResolution::Found(canonical, found) if found == expected => Ok(Some(canonical)),
        RelationResolution::Found(_, found) => Err(wrong_drop_kind_error(&local, expected, found)),
        RelationResolution::MissingSchema(schema) if if_exists => {
            notice(&format!("schema \"{schema}\" does not exist, skipping"));
            Ok(None)
        }
        RelationResolution::MissingRelation if if_exists => {
            notice(&format!("{expected} \"{local}\" does not exist, skipping"));
            Ok(None)
        }
        RelationResolution::MissingSchema(schema) => Err(SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{schema}\" does not exist"),
        }),
        RelationResolution::MissingRelation => Err(SQLError::Routine {
            sqlstate: if matches!(kind, DropKind::ForeignTable | DropKind::Index) {
                "42704"
            } else {
                "42P01"
            }
            .into(),
            message: format!("{expected} \"{local}\" does not exist"),
        }),
    }
}

pub mod hierarchy;
pub mod tables;

#[cfg(test)]
mod tests;
