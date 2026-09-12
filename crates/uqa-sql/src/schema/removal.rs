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
pub fn validate_drop_table_label_targets(
    catalog: &dyn RelationDropCatalog,
    stmt: &DropStmt,
) -> Result<(), SQLError> {
    if stmt.kind == DropKind::Table {
        for name in &stmt.names {
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
        }
    }
    Ok(())
}
pub fn bind_table_drop_targets(
    catalog: &dyn RelationDropCatalog,
    stmt: &DropStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Vec<String>, SQLError> {
    let mut tables = Vec::new();
    for name in &stmt.names {
        let (_, local) =
            uqa_core::RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
        match catalog.resolve_relation_kind(name)? {
            RelationResolution::Found(canonical, "table") => tables.push(canonical),
            RelationResolution::Found(_, _) => {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{local}\" is not a table"),
                });
            }
            RelationResolution::MissingSchema(schema) if stmt.if_exists => {
                notice(&format!("schema \"{schema}\" does not exist, skipping"));
            }
            RelationResolution::MissingRelation if stmt.if_exists => {
                notice(&format!("table \"{local}\" does not exist, skipping"));
            }
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
            RelationResolution::MissingRelation => {
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("table \"{local}\" does not exist"),
                });
            }
        }
    }
    Ok(tables)
}

pub fn bind_foreign_table_drop_targets(
    catalog: &dyn RelationDropCatalog,
    stmt: &DropStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Vec<String>, SQLError> {
    let mut foreign_tables = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in &stmt.names {
        match catalog.resolve_relation_kind(name)? {
            RelationResolution::Found(canonical, "foreign table") => {
                if seen.insert(canonical.clone()) {
                    foreign_tables.push(canonical);
                }
            }
            RelationResolution::Found(_, _) => {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{name}\" is not a foreign table"),
                });
            }
            RelationResolution::MissingSchema(schema) if stmt.if_exists => {
                notice(&format!("schema \"{schema}\" does not exist, skipping"));
            }
            RelationResolution::MissingRelation if stmt.if_exists => {
                notice(&format!(
                    "foreign table \"{name}\" does not exist, skipping"
                ));
            }
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
            RelationResolution::MissingRelation => {
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("foreign table \"{name}\" does not exist"),
                });
            }
        }
    }
    Ok(foreign_tables)
}

pub fn bind_view_drop_targets(
    catalog: &dyn RelationDropCatalog,
    stmt: &DropStmt,
) -> Result<(Vec<String>, &'static str), SQLError> {
    let expected_kind = if stmt.kind == DropKind::View {
        "view"
    } else {
        "materialized view"
    };
    let command = if stmt.kind == DropKind::View {
        "DROP VIEW"
    } else {
        "DROP MATERIALIZED VIEW"
    };
    let mut views = Vec::new();
    for name in &stmt.names {
        match catalog.resolve_relation_kind(name)?.into_found() {
            Some((canonical, kind)) if kind == expected_kind => views.push(canonical),
            Some((canonical, kind)) => {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "{command}: relation `{canonical}` is a {kind}, not a {expected_kind}"
                    ),
                });
            }
            None if stmt.if_exists => {}
            None => {
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("{command}: relation `{name}` does not exist"),
                });
            }
        }
    }
    Ok((views, expected_kind))
}

pub fn bind_sequence_drop_targets(
    catalog: &dyn RelationDropCatalog,
    stmt: &DropStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Vec<String>, SQLError> {
    let mut sequences = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in &stmt.names {
        match catalog.resolve_relation_kind(name)? {
            RelationResolution::Found(canonical, "sequence") => {
                if seen.insert(canonical.clone()) {
                    sequences.push(canonical);
                }
            }
            RelationResolution::Found(_canonical, _kind) => {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{name}\" is not a sequence"),
                });
            }
            RelationResolution::MissingRelation | RelationResolution::MissingSchema(_)
                if stmt.if_exists =>
            {
                notice(&format!("sequence \"{name}\" does not exist, skipping"));
            }
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
            RelationResolution::MissingRelation => {
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("sequence \"{name}\" does not exist"),
                });
            }
        }
    }
    Ok(sequences)
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
