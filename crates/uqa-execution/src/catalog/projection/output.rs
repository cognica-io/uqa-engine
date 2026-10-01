//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-aware output functions for projected and diagnostic values: type names follow search-path visibility and enum values print their current labels.

use crate::catalog::context::CatalogContext;
use uqa_sql::expr::enums::EnumLabelCatalog;
use uqa_sql::{expr::EngineHook, ColumnType, SQLError};

/// Output resolution over one statement's catalog view. Evaluation-only services such as sequences are unavailable to output functions.
pub struct CatalogOutput<'a>(pub CatalogContext<'a>);

impl EngineHook for CatalogOutput<'_> {
    fn resolve_regtype_output(&self, ty: &ColumnType, oid: i64) -> Result<Option<String>, String> {
        super::resolve_regtype_output(&self.0, ty, oid)
    }

    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        self.0.routines.enum_labels()
    }

    fn composite_types(&self) -> Option<&dyn uqa_sql::expr::composites::CompositeTypeCatalog> {
        self.0.routines.composite_types()
    }

    fn nextval(&self, _name: &str) -> Result<i64, SQLError> {
        Err(SQLError::Internal(
            "catalog output cannot advance a sequence".into(),
        ))
    }

    fn currval(&self, _name: &str) -> Result<i64, SQLError> {
        Err(SQLError::Internal(
            "catalog output cannot read a sequence".into(),
        ))
    }

    fn setval(&self, _name: &str, _value: i64, _is_called: bool) -> Result<i64, SQLError> {
        Err(SQLError::Internal(
            "catalog output cannot change a sequence".into(),
        ))
    }
}
