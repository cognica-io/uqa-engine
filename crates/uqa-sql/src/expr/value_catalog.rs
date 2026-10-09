//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed catalog identities and comparison support for SQL value consumers.

use super::enums::{EnumTypeComparisonStates, EnumTypeLabels};
use super::Result;
use std::sync::Arc;
use uqa_core::EnumValue;

/// Type metadata and enum support used to observe already admitted SQL values. Implementations borrow one statement catalog generation and never invoke type input.
pub trait SQLValueCatalog {
    /// The declared type for a physical OID, including domains, arrays and composites.
    fn value_type_by_oid(&self, _oid: u32) -> Result<Option<crate::ColumnType>> {
        Ok(None)
    }

    /// The current descriptor of an admitted composite value.
    fn value_composite_type(
        &self,
        _oid: u32,
    ) -> Result<Option<Arc<super::composites::CompositeTypeDescriptor>>> {
        Ok(None)
    }

    /// The session's comparison support functions, shared by declared type across nested array and record calls. Pure catalog readers do not own runtime state.
    fn enum_type_comparison_states(&self) -> Option<&EnumTypeComparisonStates> {
        None
    }

    /// The labels of one enum type in the statement's catalog generation, or `None` when the catalog has no such type.
    fn enum_type_labels(&self, type_oid: u32) -> Result<Option<Arc<EnumTypeLabels>>>;

    /// Resolve an already admitted physical label OID across enum types. Output uses the label's actual identity even if a retained tuple now declares another enum type; it does not repeat input safety checks.
    fn enum_value_by_oid(&self, _label_oid: u32) -> Result<Option<EnumValue>> {
        Ok(None)
    }

    /// The actual enum type and zero-based label position of an admitted physical OID in this generation. Positions have declaration/key order and avoid copying label keys for comparison.
    fn enum_label_position(&self, _label_oid: u32) -> Result<Option<(u32, usize)>> {
        Ok(None)
    }

    /// Whether the current transaction added this label to a type that it did not create. `PostgreSQL` rejects such a label until the transaction commits.
    fn enum_label_uncommitted(&self, label_oid: u32) -> bool;

    /// `format_type_be` of the type, which qualifies a type hidden by the search path.
    fn enum_type_name(&self, type_oid: u32) -> Result<Option<String>>;

    /// Whether the statement catalog defines any enum type; binding skips enum literal validation otherwise.
    fn has_enum_types(&self) -> bool;
}

/// Adapt a scalar context without requiring embedders that only provide domain or composite metadata to implement enum support.
pub struct EngineValueCatalog<'a>(pub &'a dyn super::EngineHook);

impl SQLValueCatalog for EngineValueCatalog<'_> {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<crate::ColumnType>> {
        self.0
            .resolve_type_oid(oid)
            .map_err(crate::SQLError::Internal)
    }

    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<super::composites::CompositeTypeDescriptor>>> {
        self.0
            .composite_types()
            .map(|catalog| catalog.composite_type(oid))
            .transpose()
            .map(Option::flatten)
    }

    fn enum_type_comparison_states(&self) -> Option<&EnumTypeComparisonStates> {
        self.0.enum_labels()?.enum_type_comparison_states()
    }

    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>> {
        self.0
            .enum_labels()
            .map(|catalog| catalog.enum_type_labels(oid))
            .transpose()
            .map(Option::flatten)
    }

    fn enum_value_by_oid(&self, oid: u32) -> Result<Option<EnumValue>> {
        self.0
            .enum_labels()
            .map(|catalog| catalog.enum_value_by_oid(oid))
            .transpose()
            .map(Option::flatten)
    }

    fn enum_label_position(&self, oid: u32) -> Result<Option<(u32, usize)>> {
        self.0
            .enum_labels()
            .map(|catalog| catalog.enum_label_position(oid))
            .transpose()
            .map(Option::flatten)
    }

    fn enum_label_uncommitted(&self, oid: u32) -> bool {
        self.0
            .enum_labels()
            .is_some_and(|catalog| catalog.enum_label_uncommitted(oid))
    }

    fn enum_type_name(&self, oid: u32) -> Result<Option<String>> {
        self.0
            .enum_labels()
            .map(|catalog| catalog.enum_type_name(oid))
            .transpose()
            .map(Option::flatten)
    }

    fn has_enum_types(&self) -> bool {
        self.0
            .enum_labels()
            .is_some_and(SQLValueCatalog::has_enum_types)
    }
}
