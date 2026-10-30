//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only view layers retire mutable constructor records while retaining every surviving payload.

use super::{OwnedAutomaticViewLayer, OwnedViewQueryShape, Produced, ProductionControl, SQLError};
use crate::{
    plan::copy::SyntaxProduction,
    semantics::view_rewrite::{AutomaticViewLayer, ViewColumn, ViewQueryShape},
};

impl OwnedAutomaticViewLayer {
    fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<AutomaticViewLayer>, SQLError> {
        control.check()?;
        let output = SyntaxProduction::new(*control);
        let mut columns = output.vector(self.columns.len())?;
        for column in output.elements(self.columns) {
            columns.push(ViewColumn {
                name: output.take(column.name),
                collation_oid: column.collation_oid,
                expression: output.take(column.expression.finish(control)?),
                writable_source_column: column.writable_source_column.map(|name| output.take(name)),
            })?;
        }
        let layer = AutomaticViewLayer {
            canonical_name: output.take(self.canonical_name),
            source_name: output.take(self.source_name),
            source_qualifier: output.take(self.source_qualifier),
            source_column_map: output.take(self.source_column_map),
            source_include_descendants: self.source_include_descendants,
            source_schema: self.source_schema,
            columns: columns.finish()?,
            predicate: self
                .predicate
                .map(|value| value.finish(control).map(|value| output.take(value)))
                .transpose()?,
            subqueries: output.take(self.subqueries.finish(control)?),
            check_option: self.check_option,
        };
        output.finish(layer)
    }
}

impl OwnedViewQueryShape {
    pub(in crate::semantics::view_rewrite) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ViewQueryShape>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let shape = match self {
            Self::Updatable(layer) => {
                let layer = output.take(layer.finish(control)?);
                ViewQueryShape::Updatable(output.boxed(layer)?)
            }
            Self::NotUpdatable(reason) => ViewQueryShape::NotUpdatable(reason),
        };
        output.finish(shape)
    }
}
