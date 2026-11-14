//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FunctionBody, FunctionParamMode, UserTypeKind};
use std::sync::{Arc, Mutex};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken, ValueRetentionError,
};

#[derive(Clone)]
struct Resolver {
    calls: Arc<Mutex<Vec<String>>>,
    fail: bool,
    dimensions: usize,
}
impl RoutineTypeIdentityInput for Resolver {
    type Error = TypeIdentityInputError;
    async fn resolve_user_type_identity(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<UserTypeIdentity>, Self::Error> {
        control.check()?;
        input.checkpoint().await?;
        self.calls.lock().unwrap().push(name.into());
        if self.fail {
            return Err(ImageIoError::Invalid.into());
        }
        let kind = match name {
            "enum_alias" => UserTypeKind::Enum,
            "domain_alias" => UserTypeKind::Domain,
            "composite_alias" => UserTypeKind::Composite,
            "range_alias" => UserTypeKind::Range,
            "multi_alias" => UserTypeKind::Multirange,
            _ => return Ok(None),
        };
        Ok(Some(UserTypeIdentity {
            kind,
            oid: 42,
            dimensions: self.dimensions,
        }))
    }
}
type Error = RoutineTypeBindingError<TypeIdentityInputError>;
type Decoder = ImageDecoder<Produced<CreateFunction>, Error>;
fn type_resolver(dimensions: usize) -> Resolver {
    Resolver {
        calls: Arc::default(),
        fail: false,
        dimensions,
    }
}
fn definition() -> CreateFunction {
    let crate::Statement::CreateFunction(value) = crate::compile(
        "CREATE FUNCTION f(a integer) RETURNS integer AS 'unparsed source' LANGUAGE SQL",
    )
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    *value
}
fn capture(value: &CreateFunction, memory: &MemoryBudget) -> Produced<CreateFunction> {
    let token = CancellationToken::new();
    crate::plan::copy::capture_stored_with_control(
        value,
        &ProductionControl::new(memory, &token, &token),
    )
    .unwrap()
}
fn decoder(
    value: Produced<CreateFunction>,
    mut resolver: Resolver,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        identify_routine_signature_types_from_input(value, &mut resolver, &control, &mut input)
            .await
    })
    .unwrap()
}
struct NoSourceReads;
impl std::io::Read for NoSourceReads {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("identity replacement consumes native declarations")
    }
}
fn advance(input: &mut Decoder, steps: usize) -> Result<Option<Produced<CreateFunction>>, Error> {
    input.advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps })
}
fn complete(input: &mut Decoder) -> Result<(Produced<CreateFunction>, usize), Error> {
    for poll in 1..100_000 {
        if let Some(value) = advance(input, 1)? {
            return Ok((value, poll));
        }
    }
    panic!("routine type binding stopped progressing")
}

fn declaration_with_type_aliases() -> CreateFunction {
    let mut value = definition();
    value.params = [
        (" domain#0007[][] ", FunctionParamMode::In),
        ("integer", FunctionParamMode::Out),
        ("enum_alias", FunctionParamMode::Out),
        ("composite_alias", FunctionParamMode::InOut),
        ("range_alias", FunctionParamMode::Variadic),
        ("multi_alias", FunctionParamMode::Table),
    ]
    .into_iter()
    .map(|(name, mode)| {
        let mut p = value.params[0].clone();
        p.type_name = name.into();
        p.mode = mode;
        p
    })
    .collect();
    value.returns = FunctionReturns::Scalar {
        type_name: "domain_alias".into(),
    };
    value
}

#[test]
fn retained_routine_type_binding_preserves_original_field_order_and_native_allocations() {
    let value = declaration_with_type_aliases();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let value = capture(&value, &native);
    let parameters = value.params.as_ptr();
    let untouched = [
        value.params[0].type_name.as_ptr(),
        value.params[1].type_name.as_ptr(),
    ];
    let FunctionBody::Source(body) = &value.body else {
        unreachable!()
    };
    let body = body.as_ptr();
    let original = value.reserved_bytes();
    let FunctionReturns::Scalar {
        type_name: old_return,
    } = &value.returns
    else {
        unreachable!()
    };
    let removed: usize = value.params[2..]
        .iter()
        .map(|p| p.type_name.capacity())
        .sum::<usize>()
        + old_return.capacity();
    let resolver = type_resolver(2);
    let calls = Arc::clone(&resolver.calls);
    let (output, _) = complete(&mut decoder(
        value, resolver, &native, &work, &token, &token,
    ))
    .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "integer",
            "enum_alias",
            "composite_alias",
            "range_alias",
            "multi_alias",
            "domain_alias"
        ]
    );
    assert_eq!(output.params.as_ptr(), parameters);
    assert_eq!(
        [
            output.params[0].type_name.as_ptr(),
            output.params[1].type_name.as_ptr()
        ],
        untouched
    );
    let FunctionBody::Source(source) = &output.body else {
        unreachable!()
    };
    assert_eq!(source.as_ptr(), body);
    assert_eq!(
        output
            .params
            .iter()
            .map(|p| p.type_name.as_str())
            .collect::<Vec<_>>(),
        [
            " domain#0007[][] ",
            "integer",
            "enum#42[][]",
            "composite#42[][]",
            "range#42[][]",
            "multirange#42[][]"
        ]
    );
    let FunctionReturns::Scalar { type_name } = &output.returns else {
        unreachable!()
    };
    assert_eq!(type_name, "domain#42[][]");
    let added = output.params[2..]
        .iter()
        .map(|p| p.type_name.capacity())
        .sum::<usize>()
        + type_name.capacity();
    assert_eq!(output.reserved_bytes(), original - removed + added);
    assert_eq!(native.used(), output.reserved_bytes());
    assert_eq!(work.used(), 0);
    token.cancel();
    assert_eq!(output.name, "f");
    drop(output);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_routine_type_binding_preserves_unselected_returns_and_lookup_failures() {
    for sql in [
        "CREATE FUNCTION f(a integer) RETURNS TABLE (x integer) AS 'body' LANGUAGE SQL",
        "CREATE FUNCTION f(a integer) RETURNS SETOF integer AS 'body' LANGUAGE SQL",
        "CREATE PROCEDURE f(a integer) AS 'body' LANGUAGE SQL",
    ] {
        let crate::Statement::CreateFunction(value) = crate::compile(sql).unwrap().remove(0) else {
            unreachable!()
        };
        let mut expected = *value;
        let mut names = Vec::new();
        for p in &expected.params {
            names.push(p.type_name.clone());
        }
        if let FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } =
            &expected.returns
        {
            names.push(type_name.clone());
        }
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let resolver = type_resolver(0);
        let calls = Arc::clone(&resolver.calls);
        let output = complete(&mut decoder(
            capture(&expected, &native),
            resolver,
            &native,
            &work,
            &token,
            &token,
        ))
        .unwrap()
        .0;
        assert_eq!(
            serde_json::to_value(&*output).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(*calls.lock().unwrap(), names);
        drop(output);
        expected.params[0].type_name = "domain_alias".into();
        let mut resolver = type_resolver(0);
        resolver.fail = true;
        let error = complete(&mut decoder(
            capture(&expected, &native),
            resolver,
            &native,
            &work,
            &token,
            &token,
        ))
        .err()
        .unwrap();
        assert!(matches!(
            error,
            Error::Lookup(TypeIdentityInputError::Work(ImageIoError::Invalid))
        ));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_routine_type_binding_retires_every_pending_replacement_and_both_cancellation_owners() {
    let mut value = definition();
    value.params = vec![value.params[0].clone(); 7];
    for p in &mut value.params {
        p.type_name = "domain_alias".into();
    }
    value.returns = FunctionReturns::SetOf {
        type_name: "domain_alias".into(),
    };
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (output, polls) = complete(&mut decoder(
        capture(&value, &native),
        type_resolver(4097),
        &native,
        &work,
        &token,
        &token,
    ))
    .unwrap();
    drop(output);
    assert!(polls > 40);
    for pending in 0..polls - 1 {
        for action in ["original", "invoking", "drop"] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(
                capture(&value, &native),
                type_resolver(4097),
                &native,
                &work,
                &original,
                &invoking,
            );
            for _ in 0..pending {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            match action {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                _ => {}
            }
            if action != "drop" {
                assert!(advance(&mut input, 1).is_err());
            }
            drop(input);
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_routine_type_binding_denial_retires_the_original_unpublished_declaration() {
    let mut value = definition();
    value.params[0].type_name = "enum_alias".into();
    for available in [0, 4, 12] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let value = capture(&value, &native);
        let hold = native.reserve(native.available() - available).unwrap();
        let error = complete(&mut decoder(
            value,
            type_resolver(4097),
            &native,
            &work,
            &token,
            &token,
        ))
        .err()
        .unwrap();
        assert!(matches!(
            error,
            Error::Input(TypeIdentityInputError::Native(ValueRetentionError::Memory(
                _
            )))
        ));
        assert_eq!(native.used(), hold.bytes());
        drop(hold);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}
