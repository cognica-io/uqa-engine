//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View DROP authorization, dependency diagnostics and temporary dependency layers.
use super::StoredView;
use crate::{
    binding::view_dependencies::query_plan_references_relation,
    catalog::security::view_ownership::{self, ViewOwnershipContext},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;

pub fn direct_view_drop_target(target: Option<(String, &str)>) -> Result<Option<String>, SQLError> {
    match target {
        Some((canonical, "view")) => Ok(Some(canonical)),
        Some((canonical, kind)) => Err(SQLError::Unsupported(format!(
            "DROP VIEW: relation `{canonical}` is a {kind}, not a view"
        ))),
        None => Ok(None),
    }
}

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

pub fn ensure_no_rule_dependents(
    names: &[String],
    dependent_rules: Vec<(RelationIdentity, String)>,
) -> Result<(), SQLError> {
    if !dependent_rules.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop view {} because other objects depend on it: {}",
                names.join(", "),
                dependent_rules
                    .into_iter()
                    .map(|(table, rule)| format!("rule {rule} on table {}", table.qualified_name()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    Ok(())
}

pub fn ensure_no_view_dependents(name: &str, dependents: &[String]) -> Result<(), SQLError> {
    if !dependents.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "DROP VIEW `{name}` rejected: dependent view(s) `{}` still reference it",
            dependents.join("`, `")
        )));
    }
    Ok(())
}

pub fn temporary_view_dependency_layers(
    canonical_name: &str,
    target: RelationIdentity,
    views: &BTreeMap<RelationIdentity, StoredView>,
) -> Result<Vec<Vec<RelationIdentity>>, String> {
    let empty_ctes = std::collections::BTreeSet::new();
    let mut targets = std::collections::BTreeSet::from([target]);
    let mut layers = Vec::new();
    loop {
        let layer = views
            .iter()
            .filter(|(relation, _)| !targets.contains(*relation))
            .filter(|(_, view)| {
                targets.iter().any(|target| {
                    query_plan_references_relation(&view.query, target, &empty_ctes)
                })
            })
            .map(|(relation, view)| {
                if view.persistence != crate::ast::RelationPersistence::Temporary {
                    return Err(format!(
                        "temporary relation `{canonical_name}` has non-temporary dependent view `{}`",
                        relation.qualified_name()
                    ));
                }
                Ok(relation.clone())
            })
            .collect::<Result<Vec<_>, String>>()?;
        if layer.is_empty() {
            break;
        }
        targets.extend(layer.iter().cloned());
        layers.push(layer);
    }
    Ok(layers)
}

#[cfg(test)]
mod tests;
