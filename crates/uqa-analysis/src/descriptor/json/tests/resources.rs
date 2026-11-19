//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn failed(input: &mut ImageDecoder<Wire, AnalysisError>) -> (AnalysisError, usize) {
    for calls in 1..100_000 {
        match input.advance(&mut NoRead, WINDOW) {
            Ok(None) => (),
            Err(error) => return (error, calls),
            Ok(Some(_)) => panic!("invalid descriptor accepted"),
        }
    }
    panic!("descriptor stopped progressing")
}

#[test]
fn descriptor_diagnostics_retain_original_name_ownership_until_redaction() {
    let secret = format!("private\0{}\0private", "한🙂".repeat(1024));
    let text: Arc<str> = format!("{{{}:null}}", serde_json::to_string(&secret).unwrap()).into();
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(text.clone(), &memory, &token);
    let (error, calls) = failed(&mut input);
    drop(input);
    let AnalysisError::JsonDiagnostic(error) = error else {
        panic!("original diagnostic lost")
    };
    assert_eq!(
        error.to_string(),
        serde_json::from_str::<Wire>(&text)
            .err()
            .unwrap()
            .to_string()
    );
    assert!(memory.used() >= secret.len());
    let position = error.position();
    let error = AnalysisError::JsonDiagnostic(error).redact_json_payloads();
    assert_eq!(memory.used(), 0);
    assert!(!format!("{error:?} {error}").contains("private"));
    let AnalysisError::JsonDiagnostic(error) = error else {
        unreachable!()
    };
    assert_eq!(error.position(), position);
    assert!(error.is_data());
    drop(error);

    for stop in 0..calls {
        for mode in 0..3 {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(4 << 20);
                let token = CancellationToken::new();
                let mut input = decoder(text.clone(), &memory, &token);
                for _ in 0..stop {
                    assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                match mode {
                    0 => (),
                    1 => {
                        token.cancel();
                        assert!(matches!(
                            failed(&mut input).0,
                            AnalysisError::Input(ImageIoError::Cancelled(_))
                        ));
                        assert!(input.advance(&mut NoRead, WINDOW).is_err());
                    }
                    _ => {
                        let admitted = memory.used() >= secret.len();
                        let held = memory.reserve((4 << 20) - memory.used()).unwrap();
                        match failed(&mut input).0 {
                            AnalysisError::Input(ImageIoError::Memory(_)) => (),
                            AnalysisError::JsonDiagnostic(error) if admitted => drop(error),
                            error => panic!("unexpected admission result at {stop}: {error}"),
                        }
                        assert!(input.advance(&mut NoRead, WINDOW).is_err());
                        drop(held);
                    }
                }
                drop(input);
                assert_eq!(memory.used(), 0, "stop {stop}, mode {mode}");
            });
            assert_eq!(measured.bytes_current, 0, "stop {stop}, mode {mode}");
        }
    }
}

struct WorkFailure {
    calls: usize,
    fail: usize,
}

impl Work<AnalysisError> for WorkFailure {
    async fn checkpoint(&mut self) -> AnalysisResult<()> {
        self.calls += 1;
        if self.calls == self.fail {
            Err(AnalysisError::Descriptor("original work error"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn descriptor_raw_value_work_failures_preserve_the_callers_original_error() {
    let protocol = uqa_core::json::JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    let mut value = original();
    let nested = serde_json::to_string(&json!({protocol.number: "1.25"})).unwrap();
    value["descriptor"]["pipeline"] = json!({protocol.raw: nested});
    let text = serde_json::to_string(&value).unwrap();
    let mut work = WorkFailure {
        calls: 0,
        fail: usize::MAX,
    };
    inline(decode(&text, &mut work)).unwrap();
    for fail in 1..=work.calls {
        let mut work = WorkFailure { calls: 0, fail };
        assert!(
            matches!(
                inline(decode(&text, &mut work)),
                Err(AnalysisError::Descriptor("original work error"))
            ),
            "checkpoint {fail}"
        );
        assert_eq!(work.calls, fail);
    }
}
