//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AsyncImageInput, InputType, Produced, ProductionControl, RoleDefinitions, SQLError,
    TypeDefinitions,
};
use uqa_sql::catalog::{
    composite_type::{validate_composite_registry_from_input, StoredComposite},
    enum_type::{validate_enum_registry_from_input, StoredEnum},
    range_type::{validate_range_registry_from_input, StoredRange},
    shell_type::{validate_shell_registry_from_input, StoredShellType},
};

impl InputType for StoredEnum {
    fn decode(text: &str, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        Self::from_stored_json_with_control(text, control)
    }
    fn oid_count(&self) -> usize {
        self.labels.len() + 2
    }
    fn oid_at(&self, index: usize) -> u32 {
        match index {
            0 => self.oid,
            1 => self.array_oid,
            _ => self.labels[index - 2].oid,
        }
    }
    async fn validate(
        registry: &TypeDefinitions<Self>,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        validate_enum_registry_from_input(registry, roles, control, work).await
    }
}

impl InputType for StoredRange {
    fn decode(text: &str, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        Self::from_stored_json_with_control(text, control)
    }
    fn oid_count(&self) -> usize {
        4
    }
    fn oid_at(&self, index: usize) -> u32 {
        self.type_oids()[index]
    }
    async fn validate(
        registry: &TypeDefinitions<Self>,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        validate_range_registry_from_input(registry, roles, control, work).await
    }
}
impl InputType for StoredComposite {
    fn decode(text: &str, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        Self::from_stored_json_with_control(text, control)
    }
    fn oid_count(&self) -> usize {
        3
    }
    fn oid_at(&self, index: usize) -> u32 {
        [self.oid, self.array_oid, self.relation_oid][index]
    }
    async fn validate(
        registry: &TypeDefinitions<Self>,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        validate_composite_registry_from_input(registry, roles, control, work).await
    }
}
impl InputType for StoredShellType {
    fn decode(text: &str, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        Self::from_stored_json_with_control(text, control)
    }
    fn oid_count(&self) -> usize {
        1
    }
    fn oid_at(&self, index: usize) -> u32 {
        assert_eq!(index, 0);
        self.oid
    }
    async fn validate(
        registry: &TypeDefinitions<Self>,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        validate_shell_registry_from_input(registry, roles, control, work).await
    }
}
