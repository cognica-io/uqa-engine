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
use crate::catalog::roles::RoleDefinitions;
use crate::{ast::RoutineAclEntry, catalog::roles::RoleIdentity, SQLError};
use std::ops::Deref;

mod registry;
pub mod stored;
pub use registry::{BuiltinRoutineSecurities, CapturedBuiltinRoutineSecurities};
mod validation;
pub use validation::validate_from_input;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinRoutineSecurity {
    pub revision: [u8; 16],
    pub execute_acl: Vec<RoutineAclEntry>,
}

pub type BuiltinRoutineSecurityRead<'a> = Box<dyn Deref<Target = BuiltinRoutineSecurities> + 'a>;

pub trait BuiltinRoutineSecurityCatalog {
    fn builtin_routine_securities(&self) -> BuiltinRoutineSecurityRead<'_>;
}

/// Authorization of an already selected built-in. Analysis may retain a binding without executing it; planners invoke this only before evaluating a constant, and executors invoke it while preparing or entering the surviving call.
pub trait BuiltinRoutineExecution: std::fmt::Debug + Send + Sync {
    fn require_execute(&self, binding: &crate::ast::FunctionBinding) -> Result<(), SQLError>;

    /// Native authority observations admit lookup and role-membership workspace before evaluating a planning constant.
    fn require_execute_with_control(
        &self,
        binding: &crate::ast::FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.require_execute(binding)?;
        control.check()?;
        Ok(())
    }
}

pub const METADATA_PREFIX: &str = "uqa.builtin_routine_security.v1:";

#[must_use]
pub fn metadata_key(oid: u32) -> String {
    format!("{METADATA_PREFIX}{oid}")
}

pub fn validate(entry: &BuiltinRoutineSecurity, roles: &RoleDefinitions) -> Result<(), SQLError> {
    validation::revision(entry)?;
    object_acl::validate_acl(
        RoleIdentity::BOOTSTRAP,
        Some(&entry.execute_acl),
        "builtin routine",
    )?;
    crate::catalog::work::inline(validation::validate_roles(
        object_acl::acl_roles(RoleIdentity::BOOTSTRAP, Some(&entry.execute_acl)).into_iter(),
        roles,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        &mut (),
    ))
}

#[cfg(test)]
mod tests;
