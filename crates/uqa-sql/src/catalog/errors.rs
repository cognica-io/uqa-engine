//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve SQL diagnostics carried through catalog storage errors.
use crate::SQLError;

pub fn storage_error(action: &str, err: &(dyn std::error::Error + 'static)) -> SQLError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(error) = source {
        if let Some(error) = error.downcast_ref::<SQLError>() {
            return SQLError::Routine {
                sqlstate: error.sqlstate().unwrap_or("XX000").into(),
                message: error.to_string(),
            };
        }
        source = error.source();
    }
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

pub fn dml_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct CatalogFailure(SQLError);
    impl std::fmt::Display for CatalogFailure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("catalog lookup failed")
        }
    }
    impl std::error::Error for CatalogFailure {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }
    #[test]
    fn catalog_error_chain_preserves_the_embedded_sql_diagnostic() {
        let original = SQLError::Routine {
            sqlstate: "42501".into(),
            message: "permission denied for schema secret".into(),
        };
        let expected = original.to_string();
        let error = storage_error("column type coercion", &CatalogFailure(original));
        assert_eq!(error.sqlstate(), Some("42501"));
        assert_eq!(error.to_string(), expected);
    }
    #[test]
    fn plain_catalog_errors_retain_the_operation_context() {
        let error = std::io::Error::other("unavailable");
        assert!(
            matches!(storage_error("column type coercion", &error), SQLError::Internal(message) if message == "column type coercion failed in storage backend: unavailable")
        );
    }
}
