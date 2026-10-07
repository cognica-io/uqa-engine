//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` ACL text rendering helpers.

pub fn acl_identifier(name: &str) -> String {
    if name.bytes().enumerate().all(|(index, byte)| {
        byte == b'_' || byte.is_ascii_lowercase() || index > 0 && byte.is_ascii_digit()
    }) {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// `aclitem` text of an ACL with one grantable privilege, such as a routine's `X` or a type's `U`: `grantee=P[*]/grantor`, with PUBLIC as an empty grantee.
pub fn object_acl_items(
    roles: &std::collections::BTreeMap<String, uqa_sql::catalog::roles::RoleDefinition>,
    acl: &[uqa_sql::ast::ObjectAclEntry],
    privilege: char,
    object_kind: &str,
) -> Result<Vec<uqa_core::Value>, uqa_sql::SQLError> {
    use uqa_sql::catalog::roles::identity::RoleSubject;
    let name = |identity: uqa_core::catalog_role::RoleIdentity| {
        identity
            .role_name(roles)
            .map(acl_identifier)
            .ok_or_else(|| {
                uqa_sql::SQLError::Internal(format!(
                    "{object_kind} ACL references a missing role incarnation"
                ))
            })
    };
    acl.iter()
        .map(|entry| {
            let grantee = entry.role.map(name).transpose()?.unwrap_or_default();
            let grantor = name(entry.grantor)?;
            Ok(uqa_core::Value::Str(format!(
                "{grantee}={privilege}{}/{grantor}",
                if entry.grant_option { "*" } else { "" }
            )))
        })
        .collect()
}
