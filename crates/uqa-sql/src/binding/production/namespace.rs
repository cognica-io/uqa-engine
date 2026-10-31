//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::BindingNameSet;
use crate::{binding::stored_relations::StoredQueryNamespace, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

pub struct ControlledStoredQueryNamespace {
    pub temporary_schema: Produced<String>,
    pub transition_relations: BindingNameSet,
}
impl ControlledStoredQueryNamespace {
    pub fn from_inputs_with_control<'a>(
        temporary_schema: Produced<String>,
        transition_relations: impl IntoIterator<Item = &'a str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut names = BindingNameSet::new();
        for name in transition_relations {
            names.insert_with_control(name, control)?;
        }
        Ok(Self {
            temporary_schema,
            transition_relations: names,
        })
    }
}
impl StoredQueryNamespace {
    pub fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledStoredQueryNamespace, SQLError> {
        ControlledStoredQueryNamespace::from_inputs_with_control(
            control.copy_text(&self.temporary_schema)?,
            self.transition_relations.iter().map(String::as_str),
            control,
        )
    }
}
