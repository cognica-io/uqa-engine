//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{identity::RoleBinding, restoration::validation};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

fn role(
    name: &str,
    oid: i64,
    control: &ProductionControl<'_>,
) -> (Budgeted<String>, Budgeted<RoleDefinition>) {
    let mut value = RoleDefinition::bootstrap();
    value.name = name.into();
    value.oid = oid;
    if oid != 10 {
        value.object_id = [1; 16];
    }
    let text = serde_json::to_string(&value).unwrap();
    (
        control.copy_text(name).unwrap().into_budgeted().unwrap(),
        RoleDefinition::from_stored_json_with_control(&text, control)
            .unwrap()
            .into_budgeted()
            .unwrap(),
    )
}

#[test]
fn stored_role_publication_keeps_native_buffers_and_independent_future_mutations() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut input = RoleDefinitionInput::new(&control).unwrap();
    let (key, value) = role("renamed", 10, &control);
    let key_address = key.as_ptr();
    let value_address = value.name.as_ptr();
    input.insert(key, value).unwrap();
    assert!(input.has_bootstrap_oid());
    let destination = MemoryBudget::new(1 << 20);
    let mut output = input.finish().unwrap().into_mutation_budget(&destination);
    assert_eq!(output.keys().next().unwrap().as_ptr(), key_address);
    assert_eq!(output["renamed"].name.as_ptr(), value_address);
    assert!(!output.contains_key("uqa"));
    let occupied = budget.reserve(budget.available()).unwrap();
    token.cancel();
    output
        .edit("renamed", |value| {
            value.connection_limit = 17;
            Ok(())
        })
        .unwrap();
    assert_eq!(output["renamed"].connection_limit, 17);
    drop(output);
    assert_eq!(destination.used(), 0);
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn stored_role_bootstrap_selection_tracks_replaced_keys_and_original_diagnostics() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut input = RoleDefinitionInput::new(&control).unwrap();
    let (key, value) = role("renamed", 10, &control);
    input.insert(key, value).unwrap();
    let (key, value) = role("renamed", 41, &control);
    input.insert(key, value).unwrap();
    assert!(!input.has_bootstrap_oid());
    let values = input.finish().unwrap();
    assert_eq!(values["uqa"], RoleDefinition::bootstrap());
    assert_eq!(values["renamed"].oid, 41);
    drop(values);
    assert_eq!(budget.used(), 0);
    let mut input = RoleDefinitionInput::new(&control).unwrap();
    let (key, value) = role("uqa", 41, &control);
    input.insert(key, value).unwrap();
    assert!(
        matches!(input.finish(), Err(SQLError::Internal(message)) if message == "persisted bootstrap role must have OID 10")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn owned_role_validation_retains_the_selected_root_and_original_allowance() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut builder = RoleDefinitionInput::new(&control).unwrap();
    for (name, oid) in [("uqa", 10), ("reader", 41)] {
        let (key, value) = role(name, oid, &control);
        builder.insert(key, value).unwrap();
    }
    let roles = builder.finish().unwrap();
    let retained = control.capture();
    let mut input = ImageDecoder::<(), SQLError>::new(&budget, &token, |mut reader| async move {
        let control = retained.control();
        let mut input = validation::begin_validation(
            &roles,
            validation::RoleValidationMode::Definitions,
            control,
        );
        loop {
            reader.input().checkpoint().await?;
            if input.advance().map_err(SQLError::from)? {
                break;
            }
        }
        Ok(())
    })
    .unwrap();
    assert!(input
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .unwrap()
        .is_none());
    input.read(&mut std::io::empty()).unwrap();
    assert_eq!(budget.used(), 0);
}

struct MembershipInput(ImageDecoder<RoleMemberships, SQLError>);
impl MembershipInput {
    fn new(
        roles: RoleDefinitions,
        values: BudgetedVec<Budgeted<RoleMembership>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let retained = control.capture();
        let memory = values.budget().clone();
        let token = CancellationToken::new();
        Ok(Self(ImageDecoder::new(
            &memory,
            &token,
            |mut reader| async move {
                restore_membership_records(&roles, values, &retained.control(), &mut reader.input())
                    .await
            },
        )?))
    }
    fn advance(&mut self) -> Result<Option<RoleMemberships>, SQLError> {
        self.0.advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        )
    }
}

fn members(control: &ProductionControl<'_>) -> BudgetedVec<Budgeted<RoleMembership>> {
    let mut values = BudgetedVec::new(control.budget().unwrap());
    for ordinal in 1..=2 {
        let bootstrap = RoleDefinition::bootstrap();
        let value = RoleMembership {
            oid: 50 + ordinal,
            role: RoleBinding::new("removed".into(), 40, [3; 16]),
            member: RoleBinding::new(
                format!("member{ordinal}"),
                u32::try_from(ordinal).unwrap() + 40,
                [u8::try_from(ordinal).unwrap(); 16],
            ),
            grantor: RoleBinding::from_definition(&bootstrap).unwrap(),
            admin_option: false,
            inherit_option: true,
            set_option: true,
        };
        let text = serde_json::to_string(&value).unwrap();
        values
            .push(
                RoleMembership::from_stored_json_with_control(&text, control)
                    .unwrap()
                    .into_budgeted()
                    .unwrap(),
            )
            .unwrap();
    }
    values
}

#[test]
fn stored_membership_input_publishes_original_names_without_rebinding_or_copying() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let values = members(&control);
    let original = values
        .iter()
        .map(|value| (value.key(), value.grantor.name.as_ptr()))
        .collect::<Vec<_>>();
    let mut input =
        MembershipInput::new(RoleDefinitions::initial_catalog(), values, &control).unwrap();
    for _ in 0..3 {
        assert!(input.advance().unwrap().is_none());
    }
    let output = input.advance().unwrap().unwrap();
    for (key, address) in original {
        assert_eq!(output[&key].grantor.name.as_ptr(), address);
    }
    assert_eq!(output.len(), 2);
    assert!(input.advance().is_err());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn stored_membership_failure_retires_partially_published_native_entries() {
    for mode in 0..3 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut input = MembershipInput::new(
            RoleDefinitions::initial_catalog(),
            members(&control),
            &control,
        )
        .unwrap();
        for _ in 0..3 {
            assert!(input.advance().unwrap().is_none());
        }
        let occupied = (mode == 0).then(|| budget.reserve(budget.available()).unwrap());
        if mode == 1 {
            original.cancel();
        }
        if mode == 2 {
            invoking.cancel();
        }
        let error = input.advance().err().unwrap();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(
            budget.used(),
            occupied
                .as_ref()
                .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        assert!(input.advance().is_err());
        drop(occupied);
        assert_eq!(budget.used(), 0);
    }
}
