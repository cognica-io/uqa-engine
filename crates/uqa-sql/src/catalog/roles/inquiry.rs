//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role privilege inquiry with ordered session and retained catalog reads.

use super::{
    guards::RoleCatalogGuards,
    memberships::{
        parse_pg_has_role_privileges, pg_has_role_privilege, resolve_pg_has_role_identifier,
        role_privilege_text,
    },
    RoleReferenceNames,
};
use crate::catalog::roles::RoleReference;
use crate::SQLError;
use uqa_core::Value;

/// Resolve an OID through the selected role catalog, preserving `PostgreSQL`'s name result and missing-role spelling.
pub fn pg_get_userbyid_value(
    catalog: &dyn RoleCatalogGuards,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let [argument] = arguments else {
        return Err(SQLError::BadArity {
            name: "pg_get_userbyid".into(),
            expected: "1".into(),
            actual: arguments.len(),
        });
    };
    let oid = match argument {
        Value::Null => return Ok(Value::Null),
        Value::Int(oid) if u32::try_from(*oid).is_ok() => *oid,
        _ => {
            return Err(SQLError::TypeMismatch(
                "pg_get_userbyid argument must be oid".into(),
            ))
        }
    };
    let roles = catalog.role_definitions();
    Ok(Value::Str(
        roles
            .values()
            .find(|role| role.oid == oid)
            .map_or_else(|| format!("unknown (OID={oid})"), |role| role.name.clone()),
    ))
}

pub fn pg_has_role_value(
    names: &dyn RoleReferenceNames,
    catalog: &dyn RoleCatalogGuards,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    if arguments.iter().any(|argument| argument == &Value::Null) {
        return Ok(Value::Null);
    }
    let (subject_value, target_value, privilege_value) = match arguments {
        [target, privilege] => (None, target, privilege),
        [subject, target, privilege] => (Some(subject), target, privilege),
        _ => {
            return Err(SQLError::BadArity {
                name: "pg_has_role".into(),
                expected: "2 or 3".into(),
                actual: arguments.len(),
            });
        }
    };
    let current_user = subject_value.is_none().then(|| names.current_role());
    let roles = catalog.role_definitions();
    let subject = subject_value.map_or_else(
        || Ok(current_user),
        |value| {
            resolve_pg_has_role_identifier(value, &roles).map(|role| role.map(RoleReference::from))
        },
    )?;
    let target = resolve_pg_has_role_identifier(target_value, &roles)?;
    let privileges = parse_pg_has_role_privileges(role_privilege_text(privilege_value)?)?;
    let memberships = catalog.role_memberships();
    let allowed = privileges.into_iter().any(|privilege| {
        pg_has_role_privilege(
            &roles,
            &memberships,
            subject.as_ref(),
            target.as_deref(),
            privilege,
        )
    });
    Ok(Value::Bool(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::roles::{
        guards::{RoleDefinitionRead, RoleMembershipRead},
        RoleDefinition,
    };
    use std::{cell::Cell, collections::BTreeMap};

    struct Catalog {
        roles: BTreeMap<String, RoleDefinition>,
        reads: Cell<usize>,
    }
    impl RoleCatalogGuards for Catalog {
        fn role_definitions(&self) -> RoleDefinitionRead<'_> {
            self.reads.set(self.reads.get() + 1);
            Box::new(&self.roles)
        }
        fn role_memberships(&self) -> RoleMembershipRead<'_> {
            panic!("role names do not depend on membership privileges")
        }
    }

    #[test]
    fn pg_get_userbyid_uses_current_identity_without_privilege_or_null_reads() {
        let mut role = RoleDefinition::bootstrap();
        role.name = "selected_owner".into();
        let mut catalog = Catalog {
            roles: BTreeMap::from([(role.name.clone(), role)]),
            reads: Cell::new(0),
        };
        assert_eq!(
            pg_get_userbyid_value(&catalog, &[Value::Null]).unwrap(),
            Value::Null
        );
        assert_eq!(catalog.reads.get(), 0);
        for (oid, expected) in [
            (10, "selected_owner"),
            (0, "unknown (OID=0)"),
            (4_294_967_295, "unknown (OID=4294967295)"),
        ] {
            assert_eq!(
                pg_get_userbyid_value(&catalog, &[Value::Int(oid)]).unwrap(),
                Value::Str(expected.into())
            );
        }
        let mut renamed = catalog.roles.remove("selected_owner").unwrap();
        renamed.name = "renamed_owner".into();
        catalog.roles.insert(renamed.name.clone(), renamed);
        assert_eq!(
            pg_get_userbyid_value(&catalog, &[Value::Int(10)]).unwrap(),
            Value::Str("renamed_owner".into())
        );
        catalog.roles.clear();
        assert_eq!(
            pg_get_userbyid_value(&catalog, &[Value::Int(10)]).unwrap(),
            Value::Str("unknown (OID=10)".into())
        );
    }
}
