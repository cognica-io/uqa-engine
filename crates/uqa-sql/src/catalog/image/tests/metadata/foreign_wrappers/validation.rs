//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::validation::{malformed, rejects};
use super::*;

#[test]
fn wrapper_images_reject_truncation_trailing_bytes_duplicate_keys_and_invalid_native_identities() {
    malformed(&singleton("w", wrapper()));
    malformed(&singleton("w", function_wrapper()));
    for corruption in 0..8 {
        let mut value = wrapper();
        match corruption {
            0 => value.identity.oid = 0,
            1 => value.identity.object_id = [0; 16],
            2 => value.identity = NativeForeignWrapper::Memory.reference(),
            3 => value.name = "other".into(),
            4 => value.owner.object_id = [0; 16],
            5 => value.options[0].0 = "bad=option".into(),
            6 => value.options[1].0 = value.options[0].0.clone(),
            _ => value.options[0].0.clear(),
        }
        rejects::<ForeignWrapperDefinition>(&encode(&singleton("w", value)));
    }
    let mut wrappers = native_wrappers();
    wrappers
        .edit("memory_fdw", |value| {
            value.handler = ForeignWrapperHandler::Native(NativeForeignWrapper::Arrow);
            Ok(())
        })
        .unwrap();
    rejects::<ForeignWrapperDefinition>(&encode(&wrappers));
}

#[test]
fn wrapper_images_reject_corrupt_callback_identities_flags_collations_and_contract_tags() {
    let bytes = encode(&singleton("w", function_wrapper()));
    assert_eq!(&bytes[87..91], &[1, 0, 1, 0]);
    for index in [91, 121, 122, 147] {
        let mut value = bytes.clone();
        value[index] = 2;
        rejects::<ForeignWrapperDefinition>(&value);
    }
    let mut value = bytes.clone();
    value[87..91].fill(0);
    rejects::<ForeignWrapperDefinition>(&value);
    value = bytes.clone();
    value[92..108].fill(0);
    rejects::<ForeignWrapperDefinition>(&value);
    value = bytes.clone();
    value[123..127].fill(255);
    rejects::<ForeignWrapperDefinition>(&value);
    value = bytes.clone();
    value[127..135].fill(255);
    rejects::<ForeignWrapperDefinition>(&value);
    value = bytes.clone();
    value[148] = 3;
    rejects::<ForeignWrapperDefinition>(&value);
    let plain = encode(&singleton("w", wrapper()));
    for (index, tag) in [(86, 3), (87, 2)] {
        let mut value = plain.clone();
        value[index] = tag;
        rejects::<ForeignWrapperDefinition>(&value);
    }
}

#[test]
fn invalid_native_callback_sources_fail_and_retire_the_shared_image_cursor() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8 << 20);
    for corruption in 0..6 {
        let mut function = callback();
        match corruption {
            0 => function.oid = 0,
            1 => function.oid = 1,
            2 => function.binding.object_id = None,
            3 => function.binding.name.clear(),
            4 => {
                function.binding.dispatch =
                    Some(crate::ast::FunctionDispatch::TypeOf { type_oid: 23 });
            }
            _ => {
                function.binding.resolution_error = Some(Box::new(
                    crate::ast::FunctionResolutionError::UndefinedFunction {
                        signature: "f(integer)".into(),
                    },
                ));
            }
        }
        let values = singleton(
            "w",
            ForeignWrapperDefinition {
                validator: Some(function),
                ..wrapper()
            },
        );
        let saved = capture(&values, &memory, &token);
        let mut reader = saved.image_reader(&token).unwrap();
        assert_eq!(
            reader.read(&mut [0; 4096]).unwrap_err().sqlstate(),
            Some("XX001")
        );
        assert!(reader.read(&mut []).is_err());
        assert!(reader.read(&mut [0; 1]).is_err());
    }
    assert_eq!(memory.used(), 0);
}
