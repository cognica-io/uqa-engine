//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine replacement compatibility, security attributes, and ALTER definition analysis.

use super::{
    builtin_routine_support_oid, lifecycle::require_routine_ownership, routine_kind,
    routine_local_name,
};
use crate::catalog::roles::identity::RoleSubject;
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
    if current_user_is_superuser {
        return Ok(());
    }
    // compute_function_attributes validates SUPPORT before CreateFunction checks LEAKPROOF.
    if def.support.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: "must be superuser to specify a support function".into(),
        });
    }
    if def.security.leakproof {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: "only superuser can define a leakproof function".into(),
        });
    }
    Ok(())
}

pub fn prepare_routine_replacement(
    existing: &CreateFunction,
    def: &mut CreateFunction,
    current_user: &(impl RoleSubject + ?Sized),
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    signature: &str,
) -> Result<(), SQLError> {
    if !def.or_replace {
        // `ProcedureCreate` names the existing routine by its unqualified name.
        let kind = routine_kind(def);
        return Err(SQLError::Routine {
            sqlstate: "42723".into(),
            message: format!(
                "{kind} \"{}\" already exists with same argument types",
                routine_local_name(&existing.name)?
            ),
        });
    }
    // `ProcedureCreate` names the routine it would replace as a function, by its unqualified name.
    require_routine_ownership(
        "function",
        &routine_local_name(&existing.name)?,
        role_inherits(
            roles,
            memberships,
            current_user,
            &crate::routines::security::bound_routine_owner(existing)?,
        ),
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
    validate_replacement_defaults(existing, def, signature)?;
    // CREATE OR REPLACE changes the definition but not object ownership or privileges.
    def.object_id = Some(existing.object_id.ok_or_else(|| {
        SQLError::Internal(format!(
            "existing routine `{}` has no catalog object identity",
            existing.name,
        ))
    })?);
    def.catalog_oid = existing.catalog_oid;
    def.owner = existing.owner;
    def.execute_acl.clone_from(&existing.execute_acl);
    Ok(())
}

/// Existing default expressions keep their result type, as `ProcedureCreate` compares `exprType` after assignment coercion. Additional defaults may precede the existing suffix; type modifiers are not type identities.
fn validate_replacement_defaults(
    existing: &CreateFunction,
    replacement: &CreateFunction,
    signature: &str,
) -> Result<(), SQLError> {
    let defaults = |definition: &CreateFunction| {
        definition
            .params
            .iter()
            .filter(|parameter| parameter.default.is_some())
            .map(|parameter| {
                parameter.default_type.as_ref().map_or(705, |ty| match ty {
                    crate::ast::RoutineDefaultType::Concrete(ty) => {
                        crate::catalog::type_metadata::pg_type_oid(ty)
                    }
                    crate::ast::RoutineDefaultType::Polymorphic(name) => {
                        crate::catalog::type_metadata::routine_type_oid(name)
                    }
                })
            })
            .collect::<Vec<_>>()
    };
    let existing_defaults = defaults(existing);
    let replacement_defaults = defaults(replacement);
    let message = if replacement_defaults.len() < existing_defaults.len() {
        "cannot remove parameter defaults from existing function"
    } else if !existing_defaults
        .iter()
        .rev()
        .zip(replacement_defaults.iter().rev())
        .all(|(existing, replacement)| existing == replacement)
    {
        "cannot change data type of existing parameter default value"
    } else {
        return Ok(());
    };
    Err(SQLError::Diagnostic {
        sqlstate: "42P13".into(),
        message: message.into(),
        detail: None,
        hint: Some(format!(
            "Use DROP {} {signature} first.",
            if existing.is_procedure {
                "PROCEDURE"
            } else {
                "FUNCTION"
            }
        )),
    })
}

/// Apply the actions of `ALTER FUNCTION` to the routine `AlterFunction` found and whose ownership it checked, in its order: the actions in written order, which a procedure may not use for its function-only attributes and none may repeat; LEAKPROOF, which needs a superuser; COST; ROWS, positive and only for a set-returning routine; the SUPPORT function; and PARALLEL. The SET actions are left for the caller to apply last.
pub fn alter_routine_attributes(
    existing: &CreateFunction,
    stmt: &AlterRoutineStmt,
    current_user_is_superuser: bool,
    authority: &dyn RoutineSupportAuthority,
) -> Result<CreateFunction, SQLError> {
    super::attributes::check_attribute_clauses(&stmt.attribute_clauses, existing.is_procedure)?;
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
    if let Some(cost) = stmt.cost {
        super::attributes::validate_cost(Some(cost))?;
        def.cost = Some(cost);
    }
    if let Some(rows) = stmt.rows {
        super::attributes::validate_rows(Some(rows))?;
        super::attributes::validate_rows_applicability(Some(rows), existing.returns_set())?;
        def.rows = Some(rows);
    }
    if let Some(support) = &stmt.support {
        validate_routine_support(authority, support)?;
        def.support = Some(support.clone());
    }
    super::attributes::validate_parallel(&stmt.attribute_clauses)?;
    if let Some(parallel) = stmt.parallel {
        def.parallel = parallel;
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

#[cfg(test)]
mod tests;
