//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound identity rendering borrows its leaf and admits one destination for every array dimension.

use super::{
    ColumnType, COMPOSITE_PREFIX, DOMAIN_PREFIX, ENUM_PREFIX, MULTIRANGE_PREFIX, RANGE_PREFIX,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

impl ColumnType {
    pub fn user_type_identity_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, ValueRetentionError> {
        control.check()?;
        let mut leaf = self;
        let mut dimensions = 0;
        while let ColumnType::Array(element) = leaf {
            control.check()?;
            leaf = element;
            dimensions += 1;
        }
        let (prefix, oid) = match leaf {
            ColumnType::Enum(reference) => (ENUM_PREFIX, reference.oid),
            ColumnType::Composite(reference) => (COMPOSITE_PREFIX, reference.oid),
            ColumnType::Domain { oid, .. } => (DOMAIN_PREFIX, *oid),
            ColumnType::DeclaredRange(reference) => (RANGE_PREFIX, reference.oid),
            ColumnType::DeclaredMultirange(reference) => (MULTIRANGE_PREFIX, reference.oid),
            _ => return Ok(None),
        };
        let leaf = control.format(format_args!("{prefix}{oid}"))?;
        let mut output = ProductionString::from_produced(leaf, *control)?;
        for _ in 0..dimensions {
            output.push_str("[]")?;
        }
        Ok(Some(output.finish()?))
    }
}
