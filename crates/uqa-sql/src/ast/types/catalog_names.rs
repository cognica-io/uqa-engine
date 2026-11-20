//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound type names preserve catalog identity and modifiers independently of `search_path`.

use super::{ColumnType, ColumnTypeInputWork};
use std::fmt;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

impl ColumnType {
    /// A search-path-independent name: user-defined OID identity or a qualified builtin catalog identifier.
    #[must_use]
    pub fn catalog_name(&self) -> String {
        self.catalog_name_with_control(&ProductionControl::uncontrolled())
            .expect("ordinary catalog name cannot be cancelled or limited")
            .into_uncontrolled()
            .expect("ordinary catalog name")
    }

    /// Format a bound identity directly into the caller's admitted destination.
    pub fn catalog_name_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        crate::catalog::work::inline(self.catalog_name_with_input_work(control, &mut ()))
    }

    pub(crate) async fn catalog_name_with_input_work<W: ColumnTypeInputWork>(
        &self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<String>, W::Error> {
        let (leaf, dimensions) = self.array_shape_with_work(control, work).await?;
        work.position(control).await?;
        let scalar = match leaf {
            Self::Named(name) => work.text(name, control).await?,
            scalar => control.format(format_args!("{}", CatalogName(scalar)))?,
        };
        Self::array_name_with_work(scalar, dimensions, control, work).await
    }

    pub(crate) async fn array_name_with_work<W: ColumnTypeInputWork>(
        scalar: Produced<String>,
        dimensions: usize,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<String>, W::Error> {
        if dimensions == 0 {
            return Ok(scalar);
        }
        let mut output = ProductionString::from_produced(scalar, *control)?;
        for _ in 0..dimensions {
            work.position(control).await?;
            output.push_str("[]")?;
        }
        Ok(output.finish()?)
    }
}

struct CatalogName<'a>(&'a ColumnType);

impl fmt::Display for CatalogName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ColumnType::Named(_) | ColumnType::Array(_) => {
                unreachable!("catalog name producer handles names and array dimensions")
            }
            ty @ (ColumnType::Enum(_)
            | ColumnType::Composite(_)
            | ColumnType::Domain { .. }
            | ColumnType::DeclaredRange(_)
            | ColumnType::DeclaredMultirange(_)) => {
                return super::identity::write_identity(ty, f);
            }
            _ => {}
        }
        let name = crate::catalog::type_metadata::builtin_type_name(self.0)
            .expect("resolved builtin type has a catalog identifier");
        write!(f, "pg_catalog.\"{name}\"")?;
        match self.0 {
            ColumnType::Varchar(Some(length))
            | ColumnType::Character(length)
            | ColumnType::Bit(Some(length))
            | ColumnType::VarBit(Some(length)) => {
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

#[cfg(test)]
mod tests;
