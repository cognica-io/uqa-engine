//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A session's temporary namespace, `pg_temp_N`, and the TOAST namespace that accompanies it, `pg_toast_temp_N`. The session's first temporary object creates both, as `InitTempTableNamespace` does, each taking the database counter's next OID before the object takes its own; a rollback past that creation forgets them, and the session's next temporary object creates them anew.

/// The name prefix of every temporary namespace, which a session's number completes.
pub const TEMPORARY_SCHEMA_PREFIX: &str = "pg_temp_";
/// The name prefix of every temporary namespace's TOAST namespace.
pub const TEMPORARY_TOAST_SCHEMA_PREFIX: &str = "pg_toast_temp_";

/// The OIDs of a session's temporary namespace and of its TOAST namespace, allocated in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemporaryNamespaceOids {
    pub namespace: u32,
    pub toast_namespace: u32,
}

/// A session's created temporary namespace as the catalog shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporaryNamespace {
    /// `pg_temp_N`.
    pub schema: String,
    pub oids: TemporaryNamespaceOids,
}

impl TemporaryNamespace {
    /// `pg_toast_temp_N`, named by the temporary namespace's number.
    pub fn toast_schema(&self) -> String {
        temporary_toast_schema_name(&self.schema)
    }

    /// The OID of `name` when it names the temporary namespace or its TOAST namespace.
    pub fn namespace_oid(&self, name: &str) -> Option<u32> {
        if name == self.schema {
            Some(self.oids.namespace)
        } else if name == self.toast_schema() {
            Some(self.oids.toast_namespace)
        } else {
            None
        }
    }

    /// Whether `oid` is the temporary namespace's or its TOAST namespace's.
    pub fn holds_oid(&self, oid: i64) -> bool {
        oid == i64::from(self.oids.namespace) || oid == i64::from(self.oids.toast_namespace)
    }
}

/// The TOAST namespace name that accompanies the temporary namespace `schema`.
pub fn temporary_toast_schema_name(schema: &str) -> String {
    let number = schema
        .strip_prefix(TEMPORARY_SCHEMA_PREFIX)
        .unwrap_or(schema);
    format!("{TEMPORARY_TOAST_SCHEMA_PREFIX}{number}")
}

/// Whether `name` names some session's temporary namespace or its TOAST namespace, as `isAnyTempNamespace` decides by the name alone.
pub fn is_temporary_schema_name(name: &str) -> bool {
    name.starts_with(TEMPORARY_SCHEMA_PREFIX) || name.starts_with(TEMPORARY_TOAST_SCHEMA_PREFIX)
}

#[cfg(test)]
mod tests;
