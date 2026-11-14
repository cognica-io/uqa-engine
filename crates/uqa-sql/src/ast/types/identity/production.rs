//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound identity rendering borrows its leaf and admits one destination for every array dimension.

use super::{ColumnType, UserTypeIdentity, UserTypeKind};
use uqa_core::{
    memory::{Produced, ProductionControl},
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
        let (kind, oid) = match leaf {
            ColumnType::Enum(reference) => (UserTypeKind::Enum, reference.oid),
            ColumnType::Composite(reference) => (UserTypeKind::Composite, reference.oid),
            ColumnType::Domain { oid, .. } => (UserTypeKind::Domain, *oid),
            ColumnType::DeclaredRange(reference) => (UserTypeKind::Range, reference.oid),
            ColumnType::DeclaredMultirange(reference) => (UserTypeKind::Multirange, reference.oid),
            _ => return Ok(None),
        };
        UserTypeIdentity {
            kind,
            oid,
            dimensions,
        }
        .render_with_control(control)
        .map(Some)
    }
}
