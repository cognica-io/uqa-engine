//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine replacement compatibility, security attributes, and ALTER definition analysis.

use super::{builtin_routine_support_oid, lifecycle::ensure_routine_owner_as, routine_kind};
use crate::{
    ast::{AlterRoutineStmt, CreateFunction},
    catalog::roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey},
    type_resolution::canonical_routine_type_name,
    SQLError,
};
use std::collections::BTreeMap;

pub trait RoutineSupportAuthority {
    fn current_user_is_superuser(&self) -> bool;
}
pub fn validate_routine_support(
    authority: &dyn RoutineSupportAuthority,
    support: &str,
) -> Result<(), SQLError> {
    if builtin_routine_support_oid(support).is_none() {
        return Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!("function {support}(internal) does not exist"),
        });
    }
    if !authority.current_user_is_superuser() {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: "must be superuser to specify a support function".into(),
        });
    }
    Ok(())
}

pub fn validate_routine_security_attributes(
    def: &CreateFunction,
    current_user_is_superuser: bool,
) -> Result<(), SQLError> {
    if (def.security.leakproof || def.support.is_some()) && !current_user_is_superuser {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: if def.security.leakproof {
                "only superuser can define a leakproof function".into()
            } else {
                "must be superuser to specify a support function".into()
            },
        });
    }
    Ok(())
}

pub fn prepare_routine_replacement(
    existing: &CreateFunction,
    def: &mut CreateFunction,
    requested_name: &str,
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Result<(), SQLError> {
    if !def.or_replace {
        let kind = routine_kind(def);
        return Err(SQLError::Routine {
            sqlstate: "42723".into(),
            message: format!("{kind} \"{requested_name}\" already exists with same argument types"),
        });
    }
    ensure_routine_owner_as(
        existing,
        role_inherits(roles, memberships, current_user, &existing.owner),
    )?;
    if existing.is_procedure != def.is_procedure {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: "cannot change routine kind".into(),
        });
    }
    if !same_return_shape(existing, def) {
        return Err(SQLError::Routine {
            sqlstate: "42P13".into(),
            message: "cannot change return type of existing function".into(),
        });
    }
    // CREATE OR REPLACE changes the definition but not object ownership or privileges.
    def.object_id = Some(existing.object_id.ok_or_else(|| {
        SQLError::Internal(format!(
            "existing routine `{}` has no catalog object identity",
            existing.name,
        ))
    })?);
    def.owner.clone_from(&existing.owner);
    def.execute_acl.clone_from(&existing.execute_acl);
    Ok(())
}

pub fn alter_routine_attributes(
    existing: &CreateFunction,
    stmt: &AlterRoutineStmt,
    current_user_is_superuser: bool,
    authority: &dyn RoutineSupportAuthority,
) -> Result<CreateFunction, SQLError> {
    if existing.is_procedure
        && (stmt.volatility.is_some()
            || stmt.strict.is_some()
            || stmt.leakproof.is_some()
            || stmt.parallel.is_some()
            || stmt.support.is_some())
    {
        return Err(SQLError::Routine {
            sqlstate: "42P13".into(),
            message: "invalid attribute in procedure definition".into(),
        });
    }

    let mut def = existing.clone();
    if let Some(volatility) = stmt.volatility {
        def.volatility = volatility;
    }
    if let Some(strict) = stmt.strict {
        def.strict = strict;
    }
    if let Some(security_definer) = stmt.security_definer {
        def.security.security_definer = security_definer;
    }
    if let Some(leakproof) = stmt.leakproof {
        if leakproof && !current_user_is_superuser {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "only superuser can define a leakproof function".into(),
            });
        }
        def.security.leakproof = leakproof;
    }
    if let Some(parallel) = stmt.parallel {
        def.parallel = parallel;
    }
    if let Some(support) = &stmt.support {
        validate_routine_support(authority, support)?;
        def.support = Some(support.clone());
    }
    def.config_actions.clone_from(&stmt.config_actions);
    Ok(def)
}

fn same_return_shape(a: &CreateFunction, b: &CreateFunction) -> bool {
    use crate::ast::FunctionReturns;
    let same_outputs = {
        let a_outs = a.output_params();
        let b_outs = b.output_params();
        a_outs.len() == b_outs.len()
            && a_outs.iter().zip(&b_outs).all(|(x, y)| {
                x.name == y.name
                    && canonical_routine_type_name(&x.type_name)
                        == canonical_routine_type_name(&y.type_name)
                    && x.mode == y.mode
            })
    };
    let same_kind = match (&a.returns, &b.returns) {
        (FunctionReturns::None, FunctionReturns::None)
        | (FunctionReturns::Table, FunctionReturns::Table) => true,
        (FunctionReturns::Scalar { type_name: x }, FunctionReturns::Scalar { type_name: y })
        | (FunctionReturns::SetOf { type_name: x }, FunctionReturns::SetOf { type_name: y }) => {
            canonical_routine_type_name(x) == canonical_routine_type_name(y)
        }
        _ => false,
    };
    same_kind && same_outputs
}
