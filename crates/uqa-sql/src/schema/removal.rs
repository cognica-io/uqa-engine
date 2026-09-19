//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP relation target binding, label protection and declared dependency discovery.
use crate::{
    ast::{DropKind, DropStmt},
    catalog::resolution::RelationResolution,
    SQLError,
};
use std::collections::BTreeSet;

pub trait RelationDropCatalog {
    fn resolve_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError>;
    fn resolve_age_label_relation_name(&self, name: &str) -> Result<Option<String>, SQLError>;
}
pub trait ForeignTableDropDependencies {
    fn views_depending_on_relation(&self, name: &str) -> Result<Vec<String>, SQLError>;
    fn rules_depending_on_relations(
        &self,
        names: &[String],
    ) -> Result<Vec<(uqa_core::RelationIdentity, String)>, SQLError>;
    fn sequence_external_dependents_for_owner_drop(
        &self,
        name: &str,
        targets: &BTreeSet<String>,
    ) -> Result<Vec<String>, SQLError>;
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
        DropKind::View => "view",
        DropKind::MaterializedView => "materialized view",
        DropKind::Sequence => "sequence",
        DropKind::Index => "index",
        DropKind::Schema => "schema",
        DropKind::Domain => "domain",
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
        RelationResolution::Found(_, _) => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{local}\" is not a {expected}"),
        }),
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

pub fn bind_relation_drop_targets(
    catalog: &dyn RelationDropCatalog,
    stmt: &DropStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Vec<String>, SQLError> {
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    for name in &stmt.names {
        if let Some(canonical) =
            bind_relation_drop_target(catalog, name, stmt.kind, stmt.if_exists, notice)?
        {
            if seen.insert(canonical.clone()) {
                targets.push(canonical);
            }
        }
    }
    Ok(targets)
}

pub fn foreign_table_drop_dependents(
    catalog: &dyn ForeignTableDropDependencies,
    foreign_tables: &[String],
    owned_sequences: &BTreeSet<String>,
    target_names: &BTreeSet<String>,
) -> Result<BTreeSet<String>, SQLError> {
    let mut dependents = std::collections::BTreeSet::new();
    for table in foreign_tables {
        dependents.extend(
            catalog
                .views_depending_on_relation(table)?
                .into_iter()
                .map(|view| format!("view {view}")),
        );
    }
    dependents.extend(
        catalog
            .rules_depending_on_relations(foreign_tables)?
            .into_iter()
            .map(|(table, rule)| format!("rule {rule} on table {}", table.qualified_name())),
    );
    for sequence in owned_sequences {
        dependents
            .extend(catalog.sequence_external_dependents_for_owner_drop(sequence, target_names)?);
    }
    Ok(dependents)
}

pub mod hierarchy;
pub mod tables;

#[cfg(test)]
mod tests;
