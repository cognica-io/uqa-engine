//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete inferred references and prepared Bind types at their distinct boundaries.

use super::{error, ColumnType, ParameterTypes, SQLError};

impl ParameterTypes<'_> {
    pub(in crate::binding::preparation) fn finish_references(
        self,
    ) -> Result<Vec<Option<ColumnType>>, SQLError> {
        self.finish_references_produced()?
            .into_uncontrolled()
            .map_err(|_| {
                SQLError::Internal("controlled parameter analysis requires its owned result".into())
            })
    }

    pub(in crate::binding::preparation) fn finish_references_produced(
        self,
    ) -> Result<uqa_core::memory::Produced<Vec<Option<ColumnType>>>, SQLError> {
        let control = self.control.control();
        for (index, observed) in self.occurrences {
            control.check()?;
            if observed.as_deref() != self.types[index].as_deref() {
                return Err(error(
                    "42P08",
                    format!("could not determine data type of parameter ${}", index + 1),
                ));
            }
        }
        let mut output = uqa_core::memory::ProductionVec::new(control);
        output.reserve(self.types.len())?;
        for ty in self.types {
            let (ty, memory) = ty.map_or((None, control.empty_reservation()), |ty| {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            });
            output.push_produced(control.finish(ty, memory)?)?;
        }
        Ok(output.finish()?)
    }

    pub(in crate::binding::preparation) fn finish(
        self,
    ) -> Result<Vec<Option<ColumnType>>, SQLError> {
        self.finish_produced()?.into_uncontrolled().map_err(|_| {
            SQLError::Internal("controlled parameter analysis requires its owned result".into())
        })
    }

    pub(in crate::binding::preparation) fn finish_produced(
        self,
    ) -> Result<uqa_core::memory::Produced<Vec<Option<ColumnType>>>, SQLError> {
        let types = self.finish_references_produced()?;
        if let Some(index) = types.iter().position(Option::is_none) {
            return Err(error(
                "42P18",
                format!("could not determine data type of parameter ${}", index + 1),
            ));
        }
        Ok(types)
    }
}
