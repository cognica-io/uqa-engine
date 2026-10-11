//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound type names preserve catalog identity and modifiers independently of `search_path`.

use super::ColumnType;
use std::fmt;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

impl ColumnType {
    /// A search-path-independent name: user-defined OID identity or a qualified builtin catalog identifier.
    #[must_use]
    pub fn catalog_name(&self) -> String {
        CatalogName(self).to_string()
    }

    /// Format a bound identity directly into the caller's admitted destination.
    pub fn catalog_name_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        control.format(format_args!("{}", CatalogName(self)))
    }
}

struct CatalogName<'a>(&'a ColumnType);

impl fmt::Display for CatalogName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ColumnType::Named(name) => return f.write_str(name),
            ColumnType::Array(element) => return write!(f, "{}[]", Self(element)),
            ty @ (ColumnType::Enum(_) | ColumnType::Composite(_) | ColumnType::Domain { .. }) => {
                return super::identity::write_identity(ty, f);
            }
            _ => {}
        }
        let name = crate::catalog::type_metadata::builtin_type_name(self.0)
            .expect("resolved builtin type has a catalog identifier");
        write!(f, "pg_catalog.\"{name}\"")?;
        match self.0 {
            ColumnType::Varchar(Some(length)) | ColumnType::Character(length) => {
                write!(f, "({length})")
            }
            ColumnType::Numeric {
                precision: Some(precision),
                scale: Some(scale),
            } => write!(f, "({precision},{scale})"),
            ColumnType::TimePrecision(precision)
            | ColumnType::TimeTzPrecision(precision)
            | ColumnType::TimestampPrecision(precision)
            | ColumnType::TimestampTzPrecision(precision) => write!(f, "({precision})"),
            ColumnType::IntervalWithFields { fields, precision } => {
                let mask = fields.modifier_mask();
                match precision {
                    Some(precision) => write!(f, "({mask},{precision})"),
                    None => write!(f, "({mask})"),
                }
            }
            ColumnType::Vector(dimension) | ColumnType::Tensor(dimension) => {
                write!(f, "({dimension})")
            }
            _ => Ok(()),
        }
    }
}
