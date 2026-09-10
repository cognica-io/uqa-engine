//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain DROP name resolution, ownership checks, and atomic dependency deletion.

use std::collections::BTreeSet;

use uqa_sql::ast::{ColumnType, DropStmt};
use uqa_sql::SQLError;

use crate::schema_security::SchemaAclPrivilege;
use crate::Engine;

impl Engine {
    pub(crate) fn drop_domains_sql(&self, statement: &DropStmt) -> Result<(), SQLError> {
        self.synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        let mut targets = BTreeSet::new();
        for name in &statement.names {
            if let Some(oid) = self.resolve_drop_domain(name, statement.if_exists)? {
                targets.insert(oid);
            }
        }
        self.drop_domain_types_and_routines(&targets, statement.cascade)
    }

    fn resolve_drop_domain(&self, name: &str, if_exists: bool) -> Result<Option<u32>, SQLError> {
        let parsed = uqa_sql::parse_regtype_name(name)?
            .ok_or_else(|| SQLError::Internal("DROP DOMAIN has no type name".into()))?;
        let label = format!(
            "{}{}",
            parsed.names.join("."),
            "[]".repeat(parsed.array_dimensions)
        );
        if let [schema, _] = parsed.names.as_slice() {
            if self.schema_security_for_privilege(schema).is_none() {
                if if_exists {
                    self.push_sql_notice(
                        "NOTICE",
                        &format!("schema \"{schema}\" does not exist, skipping"),
                    );
                    return Ok(None);
                }
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
            self.require_schema_privilege(
                schema,
                &self.current_user_name(),
                SchemaAclPrivilege::Usage,
            )?;
        }
        let oid = crate::sql::resolve_regobject_oid(self, &ColumnType::Regtype, name)?;
        let Some(oid) = oid else {
            if if_exists {
                self.push_sql_notice(
                    "NOTICE",
                    &format!("type \"{label}\" does not exist, skipping"),
                );
                return Ok(None);
            }
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("type \"{label}\" does not exist"),
            });
        };
        let domain = u32::try_from(oid)
            .ok()
            .and_then(|oid| self.domain_by_oid(oid))
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("\"{label}\" is not a domain"),
            })?;
        let owns_schema = self
            .schema_security_for_privilege(&domain.identity.schema)
            .is_some_and(|security| self.current_user_has_role_privileges(&security.role_owner));
        if !owns_schema && !self.current_user_has_role_privileges(&domain.owner) {
            let label = crate::sql::resolve_regtype_output(self, &ColumnType::Regtype, oid)
                .map_err(SQLError::Internal)?
                .ok_or_else(|| SQLError::Internal("DROP DOMAIN target disappeared".into()))?;
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("must be owner of type {label}"),
            });
        }
        Ok(Some(domain.oid))
    }
}
