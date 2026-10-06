//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! EXECUTE ACLs of immutable catalog routines. Their owner and routine identity
//! stay pinned; ACL endpoints retain role incarnations like user routine ACLs.

pub mod initialization;
pub mod selection;

#[derive(Clone, Copy, Debug)]
pub struct BuiltinRoutineIdentity {
    pub oid: u32,
    pub name: &'static str,
    pub kind: char,
    pub argument_types: &'static [i64],
}

use super::object_acl;
use crate::catalog::roles::identity::RoleSubject;
use crate::{
    ast::RoutineAclEntry,
    catalog::roles::{RoleDefinition, RoleIdentity},
    SQLError,
};
use std::{collections::BTreeMap, ops::Deref};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinRoutineSecurity {
    pub revision: [u8; 16],
    pub execute_acl: Vec<RoutineAclEntry>,
}

pub type BuiltinRoutineSecurities = BTreeMap<u32, BuiltinRoutineSecurity>;
pub type BuiltinRoutineSecurityRead<'a> = Box<dyn Deref<Target = BuiltinRoutineSecurities> + 'a>;

pub trait BuiltinRoutineSecurityCatalog {
    fn builtin_routine_securities(&self) -> BuiltinRoutineSecurityRead<'_>;
}

/// Authorization of an already selected built-in. Analysis may retain a binding without executing it; planners invoke this only before evaluating a constant, and executors invoke it while preparing or entering the surviving call.
pub trait BuiltinRoutineExecution: std::fmt::Debug + Send + Sync {
    fn require_execute(&self, binding: &crate::ast::FunctionBinding) -> Result<(), SQLError>;
}

pub const METADATA_PREFIX: &str = "uqa.builtin_routine_security.v1:";

#[must_use]
pub fn metadata_key(oid: u32) -> String {
    format!("{METADATA_PREFIX}{oid}")
}

pub fn validate(
    entry: &BuiltinRoutineSecurity,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    if entry.revision == [0; 16] {
        return Err(SQLError::Internal(
            "invalid builtin routine ACL tuple identity".into(),
        ));
    }
    object_acl::validate_acl(
        RoleIdentity::BOOTSTRAP,
        Some(&entry.execute_acl),
        "builtin routine",
    )?;
    for role in object_acl::acl_roles(RoleIdentity::BOOTSTRAP, Some(&entry.execute_acl)) {
        if role.role_name(roles).is_none() {
            return Err(SQLError::Internal(
                "builtin routine ACL references missing role incarnation".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
