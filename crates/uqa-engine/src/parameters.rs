//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement and retained-resource adapters for catalog-aware host value input.

use crate::Engine;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{SQLError, SQLParam};

impl Engine {
    /// Create a typed parameter from external text, or a typed SQL NULL from `None`. Type input runs with the session's current catalog, privileges and presentation settings. Domain checks and input callbacks share native transaction admission, commit and failure recovery; the returned parameter retains the supplied production allowance.
    pub fn create_typed_parameter_with_control(
        &self,
        type_name: &str,
        text: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<SQLParam>, SQLError> {
        self.with_direct_query_snapshot(
            false,
            |engine| {
                uqa_sql::params::create_typed_parameter_with_control(
                    type_name, text, engine, engine, control,
                )
            },
            std::convert::identity,
        )
    }
}
