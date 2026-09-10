//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native view and foreign-table alteration targets and relation rename declarations.
use crate::{
    ast::{
        AlterForeignTableAction, AlterForeignTableStmt, AlterViewAction, AlterViewKind,
        AlterViewStmt,
    },
    catalog::resolution::{resolve_relation_rename_source, RelationResolution},
    SQLError,
};
use uqa_core::RelationIdentity;

pub trait RelationAlterNames {
    fn resolve_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError>;
    fn relation_kind_at(&self, name: &str) -> Result<Option<&'static str>, String>;
}

pub struct ViewAlterTarget {
    pub canonical: String,
    pub relation: RelationIdentity,
    pub kind: &'static str,
}

pub fn view_alter_target(
    resolution: RelationResolution,
    statement: &AlterViewStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Option<ViewAlterTarget>, SQLError> {
    let kind = match statement.kind {
        AlterViewKind::View => "view",
        AlterViewKind::MaterializedView => "materialized view",
    };
    let resolution = if matches!(statement.action, AlterViewAction::RenameTo(_)) {
        resolve_relation_rename_source(resolution, &statement.name, statement.if_exists, notice)?
    } else {
        resolution.into_found()
    };
    let Some((canonical, actual_kind)) = resolution else {
        if statement.if_exists {
            return Ok(None);
        }
        return Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: format!("relation \"{}\" does not exist", statement.name),
        });
    };
    if actual_kind != kind {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{}\" is not a {kind}", statement.name),
        });
    }
    let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
        SQLError::Internal(format!("invalid ALTER VIEW target `{canonical}`: {error}"))
    })?;
    Ok(Some(ViewAlterTarget {
        canonical,
        relation,
        kind,
    }))
}

pub fn foreign_table_alter_target(
    resolution: RelationResolution,
    statement: &AlterForeignTableStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Option<String>, SQLError> {
    let resolution = if matches!(statement.action, AlterForeignTableAction::RenameTo(_)) {
        let Some((canonical, kind)) = resolve_relation_rename_source(
            resolution,
            &statement.name,
            statement.if_exists,
            notice,
        )?
        else {
            return Ok(None);
        };
        RelationResolution::Found(canonical, kind)
    } else {
        resolution
    };
    match resolution {
        RelationResolution::Found(canonical, "foreign table") => Ok(Some(canonical)),
        RelationResolution::Found(_, _) => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{}\" is not a foreign table", statement.name),
        }),
        RelationResolution::MissingSchema(schema) if statement.if_exists => {
            notice(&format!("schema \"{schema}\" does not exist, skipping"));
            Ok(None)
        }
        RelationResolution::MissingRelation if statement.if_exists => {
            notice(&format!(
                "foreign table \"{}\" does not exist, skipping",
                statement.name
            ));
            Ok(None)
        }
        RelationResolution::MissingSchema(schema) => Err(SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{schema}\" does not exist"),
        }),
        RelationResolution::MissingRelation => Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: format!("foreign table \"{}\" does not exist", statement.name),
        }),
    }
}

pub fn relation_rename_target(
    catalog: &dyn RelationAlterNames,
    source: &RelationIdentity,
    new_name: &str,
    context: &str,
) -> Result<RelationIdentity, SQLError> {
    let (schema, local_name) = RelationIdentity::parse_reference(new_name)
        .map_err(|error| SQLError::Internal(format!("invalid {context} target: {error}")))?;
    if schema.is_some() {
        return Err(SQLError::Internal(format!(
            "{context} produced a qualified target"
        )));
    }
    let target = RelationIdentity::new(&source.schema, local_name);
    if target == *source
        || catalog
            .relation_kind_at(&target.qualified_name())
            .map_err(|error| {
                SQLError::Internal(format!(
                    "check {context} target `{}`: {error}",
                    target.qualified_name()
                ))
            })?
            .is_some()
    {
        return Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{}\" already exists", target.name),
        });
    }
    Ok(target)
}

pub fn set_view_options(options: &mut Vec<(String, String)>, changes: &[(String, String)]) {
    for (name, value) in changes {
        options.retain(|(current, _)| current != name);
        options.push((name.clone(), value.clone()));
    }
}

pub fn reset_view_options(options: &mut Vec<(String, String)>, names: &[String]) {
    options.retain(|(current, _)| !names.contains(current));
}
