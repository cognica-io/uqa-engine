//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine removal identities, dependency results, and declaration diagnostics.

pub mod binding;
pub mod dependencies;
pub mod diagnostics;
pub mod lookup;
pub mod names;
pub mod relations;
pub mod rename;
pub mod restoration;
pub mod rewrites;

use super::SQLUserFunction;
use crate::{
    ast::{AlterRoutineKind, CreateFunction, FunctionBinding},
    SQLError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uqa_core::RelationIdentity;

pub type RoutineRegistry = BTreeMap<String, Vec<Arc<SQLUserFunction>>>;

pub struct SQLFunctionDropPlan {
    pub domains: BTreeSet<u32>,
    pub targets: Vec<RoutineDropTarget>,
    pub dependents: RoutineObjectDependents,
    pub notices: Vec<(&'static str, String)>,
}

#[derive(Default)]
pub struct RoutineDropResolution {
    pub targets: Vec<RoutineDropTarget>,
    pub seen_targets: BTreeSet<RoutineDropTarget>,
    pub notices: Vec<(&'static str, String)>,
}

pub struct RoutineObjectDependents {
    pub indexes: Vec<RelationIdentity>,
    pub views: Vec<String>,
    pub columns: Vec<(String, String, bool)>,
    pub defaults: Vec<(String, String, bool)>,
    pub checks: Vec<(String, String, bool)>,
    pub triggers: Vec<(String, String)>,
    pub rules: Vec<(String, String)>,
}

#[derive(Default)]
pub struct RoutineSchemaDependents {
    pub columns: Vec<(String, String, bool)>,
    pub defaults: Vec<(String, String, bool)>,
    pub checks: Vec<(String, String, bool)>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RoutineDropTarget {
    pub object_id: Option<[u8; 16]>,
    pub name: String,
    pub argument_types: Vec<String>,
    pub is_procedure: bool,
}

impl RoutineDropTarget {
    pub fn kind(&self) -> &'static str {
        if self.is_procedure {
            "procedure"
        } else {
            "function"
        }
    }

    pub fn label(&self) -> String {
        routine_signature_label(&self.name, &self.argument_types)
    }

    pub fn binding(&self) -> FunctionBinding {
        FunctionBinding {
            object_id: self.object_id,
            name: self.name.clone(),
            argument_types: self.argument_types.clone(),
            builtin: false,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        }
    }
}

pub fn routine_signature_label(name: &str, types: &[String]) -> String {
    let display_types = types
        .iter()
        .map(|type_name| {
            crate::ast::ColumnType::from_sql_name(type_name)
                .map_or_else(|_| type_name.clone(), |column_type| column_type.sql_name())
        })
        .collect::<Vec<_>>();
    format!("{name}({})", display_types.join(", "))
}

/// `func_signature_string`: the routine name as the command wrote it and each argument type as `format_type_be` spells it.
pub fn routine_signature_display(
    catalog: &dyn names::RoutineNameCatalog,
    name: &str,
    types: &[String],
) -> String {
    let types = types
        .iter()
        .map(|type_name| catalog.routine_type_display(type_name))
        .collect::<Vec<_>>();
    format!("{name}({})", types.join(", "))
}

/// `LookupFuncWithArgs`: the routine an argument list selects is not of the kind the command names.
pub fn wrong_routine_kind_error(signature: &str, expected_kind: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42809".into(),
        message: format!("{signature} is not a {expected_kind}"),
    }
}

/// `LookupFuncWithArgs`: a name without an argument list selects more than one routine of the command's kind.
pub fn ambiguous_routine_error(kind: &str, name: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42725".into(),
        message: format!("{kind} name \"{name}\" is not unique"),
        detail: None,
        hint: Some(format!(
            "Specify the argument list to select the {kind} unambiguously."
        )),
    }
}

pub fn alter_routine_kind_name(kind: AlterRoutineKind) -> &'static str {
    match kind {
        AlterRoutineKind::Function => "function",
        AlterRoutineKind::Procedure => "procedure",
        AlterRoutineKind::Routine => "routine",
    }
}

pub fn alter_routine_kind_matches(kind: AlterRoutineKind, def: &CreateFunction) -> bool {
    match kind {
        AlterRoutineKind::Function => !def.is_procedure,
        AlterRoutineKind::Procedure => def.is_procedure,
        AlterRoutineKind::Routine => true,
    }
}

pub fn ensure_routine_owner_as(
    definition: &CreateFunction,
    current_user_has_owner_privileges: bool,
) -> Result<(), SQLError> {
    if current_user_has_owner_privileges {
        Ok(())
    } else {
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "must be owner of {} {}",
                if definition.is_procedure {
                    "procedure"
                } else {
                    "function"
                },
                definition.name
            ),
        })
    }
}
