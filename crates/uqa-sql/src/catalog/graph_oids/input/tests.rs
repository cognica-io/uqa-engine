//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{decode_image, ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

const LABEL: &str = r#"{"sequence":3,"relation":{"relation":4,"row_type":5,"array_type":6},"id_default":7,"properties_default":8,"not_null":{"id":9,"properties":10},"toast_table":11,"toast_index":12,"primary_key":{"index":13,"constraint":14},"endpoint_indexes":{"start_id":15,"end_id":16},"trigger":17}"#;

fn record(labels: &str) -> String {
    format!(r#"{{"namespace":1,"label_sequence":2,"labels":{{{labels}}}}}"#)
}

fn read(text: &str, memory: &MemoryBudget) -> Result<Budgeted<GraphCatalogOids>, SQLError> {
    let token = CancellationToken::new();
    decode_image(&mut std::io::empty(), memory, &token, async |mut input| {
        let control = ProductionControl::new(memory, &token, &token);
        decode(text, &control, &mut input).await
    })
}

fn compare(text: &str) {
    let expected = serde_json::from_str::<GraphCatalogOids>(text);
    let memory = MemoryBudget::new(16 << 20);
    let actual = read(text, &memory);
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?}, {expected:?}"
    );
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(*actual, expected, "{text}");
    }
    assert_eq!(memory.used(), 0, "{text}");
}

#[test]
fn stored_graph_oid_input_preserves_named_positional_and_optional_field_shapes() {
    let base = record(&format!(r#""17":{LABEL}"#));
    compare(&base);
    compare(&record(""));
    compare("[1,2,{}]");
    compare(&format!(
        r#"[1,2,{{"17":[3,[4,5,6],7,8,{{"id":9}},11,12,[13,14],[15,16],17]}}]"#
    ));
    for source in [
        LABEL.to_owned(),
        LABEL.replace(r#","primary_key":{"index":13,"constraint":14}"#, ""),
        LABEL.replace(
            r#""primary_key":{"index":13,"constraint":14}"#,
            r#""primary_key":null"#,
        ),
        LABEL.replace(
            r#""endpoint_indexes":{"start_id":15,"end_id":16}"#,
            r#""endpoint_indexes":null"#,
        ),
        LABEL.replace(
            r#"{"relation":4,"row_type":5,"array_type":6}"#,
            r#"{"relation":4}"#,
        ),
        LABEL.replace(r#"{"relation":4,"row_type":5,"array_type":6}"#, "[4]"),
        LABEL.replace(
            r#"{"relation":4,"row_type":5,"array_type":6}"#,
            "[4,null,null,null]",
        ),
        LABEL.replace("\"id\":9", r#""한글\uD83D\uDE00":9"#),
    ] {
        compare(&record(&format!(r#""17":{source}"#)));
    }
    let value: serde_json::Value = serde_json::from_str(LABEL).unwrap();
    for field in value.as_object().unwrap().keys() {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        compare(&record(&format!(r#""0":{missing}"#)));
        let mut unknown = value.clone();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), serde_json::Value::Null);
        compare(&record(&format!(r#""0":{unknown}"#)));
        compare(&record(&format!(
            r#""0":{{"{field}":null,{}}}"#,
            &LABEL[1..LABEL.len() - 1]
        )));
    }
    for text in [
        "null",
        "{}",
        "[]",
        "[1,2]",
        "[1,2,{},0]",
        r#"{"namespace":1,"namespace":2,"label_sequence":3,"labels":{}}"#,
        r#"{"namespace":1,"label_sequence":2,"labels":{},"x":null}"#,
    ] {
        compare(text);
    }
}

#[test]
fn stored_graph_oid_input_preserves_numeric_classes_and_ordered_duplicate_maps() {
    for literal in [
        "0",
        "1",
        "4294967295",
        "4294967296",
        "-0",
        "-1",
        "1.0",
        "1e0",
        "null",
        "true",
        "\"1\"",
        "[]",
    ] {
        compare(&record("").replace("\"namespace\":1", &format!("\"namespace\":{literal}")));
    }
    for key in [
        "0",
        "1",
        "4294967295",
        "4294967296",
        "-0",
        "-1",
        "+1",
        "01",
        "1.0",
        "1e0",
        " 1",
        "",
        "\\u0031",
        "999999999999999999999999999",
    ] {
        compare(&record(&format!(r#""{key}":{LABEL}"#)));
    }
    let replacement = LABEL
        .replace("\"id\":9", r#""id":99,"\u0069d":101,"id":102"#)
        .replace("\"trigger\":17", "\"trigger\":31");
    let text = record(&format!(r#""17":{LABEL},"0":{LABEL},"17":{replacement}"#));
    compare(&text);
    let memory = MemoryBudget::new(1 << 20);
    let actual = read(&text, &memory).unwrap();
    assert_eq!(actual.labels[&17].trigger, 31);
    assert_eq!(actual.labels[&17].not_null["id"], 102);
    assert_eq!(actual.labels.keys().copied().collect::<Vec<_>>(), [0, 17]);
    drop(actual);
    assert_eq!(memory.used(), 0);
    compare(&record(&format!(r#""17":null,"17":{LABEL}"#)));
    compare(&record(&format!(r#""17":{LABEL},"17":null"#)));
}

#[test]
fn stored_graph_oid_input_rejects_truncation_and_keeps_exact_native_allocation_owners() {
    let label = LABEL.replace("\"id\":9", &format!("\"{}\":9", "한글".repeat(256)));
    let text = record(&format!(r#""17":{label},"17":{LABEL},"3":{label}"#));
    for (end, _) in text.char_indices() {
        let memory = MemoryBudget::new(1 << 20);
        assert!(read(&text[..end], &memory).is_err(), "{end}");
        assert_eq!(memory.used(), 0, "{end}");
    }
    for suffix in ["x", "0", "{}"] {
        compare(&format!("{text}{suffix}"));
    }
    let memory = MemoryBudget::new(1 << 20);
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(read(&text, &memory).unwrap());
    });
    assert_eq!(
        allocations.bytes_current,
        i64::try_from(memory.used()).unwrap()
    );
    let retained = memory.used();
    drop(output);
    assert_eq!(memory.used(), 0);
    for limit in [0, 1, 128, retained - 1] {
        let memory = MemoryBudget::new(limit);
        assert!(read(&text, &memory).is_err());
        assert_eq!(memory.used(), 0);
    }
}

fn decoder(
    text: String,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<Budgeted<GraphCatalogOids>, SQLError> {
    let memory_owner = memory.clone();
    let original_owner = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&memory_owner, &original_owner, input.cancellation);
        decode(&text, &control, &mut input).await
    })
    .unwrap()
}

#[test]
fn stored_graph_oid_input_resumes_long_tokens_and_retires_every_terminal_owner() {
    let label = LABEL.replace("\"id\":9", &format!("\"{}\":9", "한글😀".repeat(8192)));
    let text = record(&format!(r#""17":{label},"17":{LABEL},"3":{label}"#));
    let expected: GraphCatalogOids = serde_json::from_str(&text).unwrap();
    let mut counts = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(4 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(text.clone(), &memory, &token, &token);
        for count in 1..65536 {
            if let Some(value) = input
                .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
                .unwrap()
            {
                assert_eq!(*value, expected);
                counts.push(count);
                drop(value);
                break;
            }
        }
        assert!(input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
            .is_err());
        assert_eq!(memory.used(), 0);
    }
    assert!(counts[0] > counts[1] && counts[1] > counts[2]);
    for cut in [1, 8, 64, 128] {
        for terminal in ["original", "invoking", "zero", "memory", "drop", "unwind"] {
            let memory = MemoryBudget::new(4 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(text.clone(), &memory, &original, &invoking);
            for _ in 0..cut {
                assert!(input
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .unwrap()
                    .is_none());
            }
            let occupied = if terminal == "memory" {
                Some(memory.reserve(memory.available()).unwrap())
            } else {
                None
            };
            match terminal {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("retire graph input");
                    }));
                    assert_eq!(memory.used(), 0);
                    continue;
                }
                _ => (),
            }
            if terminal != "drop" {
                loop {
                    match input.advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow {
                            bytes: 1,
                            steps: usize::from(terminal != "zero"),
                        },
                    ) {
                        Err(_) => break,
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("terminal input completed: {terminal}"),
                    }
                }
            }
            drop((input, occupied));
            assert_eq!(memory.used(), 0, "{terminal}/{cut}");
        }
    }
}
