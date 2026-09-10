//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Establish an independently authenticated database session.

use uqa_sql::ast::RoleAttribute;
use uqa_sql::SQLError;

use crate::database_security::DatabaseAclPrivilege;
use crate::Engine;

impl Engine {
    /// Open an independent session for a role authenticated by the embedding host. This checks role existence, LOGIN, and database CONNECT privileges; credential verification and connection limits belong to the host's connection manager.
    pub fn new_session_for_user(&self, user: &str) -> Result<Self, SQLError> {
        let session = self
            .new_session()
            .map_err(|error| SQLError::Internal(format!("open authenticated session: {error}")))?;
        {
            let roles = session.durable.roles.read();
            let role = roles.get(user).ok_or_else(|| SQLError::Routine {
                sqlstate: "28000".into(),
                message: format!("role \"{user}\" does not exist"),
            })?;
            if !role.has(RoleAttribute::Login) {
                return Err(SQLError::Routine {
                    sqlstate: "28000".into(),
                    message: format!("role \"{user}\" is not permitted to log in"),
                });
            }
        }
        session.ensure_database_privilege(user, DatabaseAclPrivilege::Connect)?;
        {
            let mut state = session.session.state.write();
            state.session_user = user.to_string();
            state.current_user = user.to_string();
            state.sql_statement_cache.clear();
        }
        Ok(session)
    }
}
