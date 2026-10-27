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
        for (index, observed) in self.occurrences {
            if observed != self.types[index] {
                return Err(error(
                    "42P08",
                    format!("could not determine data type of parameter ${}", index + 1),
                ));
            }
        }
        Ok(self.types)
    }

    pub(in crate::binding::preparation) fn finish(
        self,
    ) -> Result<Vec<Option<ColumnType>>, SQLError> {
        let types = self.finish_references()?;
        if let Some(index) = types.iter().position(Option::is_none) {
            return Err(error(
                "42P18",
                format!("could not determine data type of parameter ${}", index + 1),
            ));
        }
        Ok(types)
    }
}
