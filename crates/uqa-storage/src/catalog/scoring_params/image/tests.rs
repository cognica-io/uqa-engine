//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn wire(rows: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = b"UQASCP01".to_vec();
    bytes.extend_from_slice(&(rows.len() as u64).to_le_bytes());
    for (name, value) in rows {
        for text in [name, value] {
            bytes.extend_from_slice(&(text.len() as u64).to_le_bytes());
            bytes.extend_from_slice(text.as_bytes());
        }
    }
    bytes
}

fn bytes(captured: &CapturedScoringParameters) -> Vec<u8> {
    let output = StorageReadControl::with_limit(4096);
    let mut reader = captured.image_reader(&output).unwrap();
    let mut bytes = Vec::new();
    let mut part = [0; 19];
    loop {
        let count = reader.read(&mut part).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&part[..count]);
    }
    assert_eq!(output.memory().used(), 0);
    bytes
}

#[test]
fn scoring_parameter_images_preserve_opaque_bytes_and_native_generations() {
    let rows = [
        ("", ""),
        ("signal\0x", "{ \"x\": 1.00, \"x\": 2 }"),
        ("한글", "opaque\0\n"),
    ];
    let source = StorageReadControl::with_limit(1 << 20);
    let capture = StorageReadControl::with_limit(1 << 20);
    let restored = StorageReadControl::with_limit(1 << 20);
    let mut parameters = ScoringParameters::new(source.memory());
    for (name, value) in rows {
        parameters.insert(name.into(), value.into()).unwrap();
    }
    let captured = parameters.capture(&capture).unwrap();
    parameters
        .replace([("replacement".into(), "later".into())])
        .unwrap();
    drop(parameters);
    let expected = wire(&rows);
    assert_eq!(bytes(&captured), expected);
    let decoded = ScoringParameters::read_image(&mut expected.as_slice(), &restored).unwrap();
    assert!(decoded
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .eq(rows));
    let mut current = decoded.clone();
    let saved = decoded.capture(&capture).unwrap();
    drop(decoded);
    current.remove("").unwrap();
    current.insert("later".into(), "value".into()).unwrap();
    drop(current);
    assert!(restored.memory().used() > 0);
    assert_eq!(bytes(&saved), expected);
    drop((captured, saved));
    assert_eq!(source.memory().used(), 0);
    assert_eq!(capture.memory().used(), 0);
    assert_eq!(restored.memory().used(), 0);
}

#[test]
fn scoring_parameter_images_reject_malformed_partial_and_unadmitted_input() {
    let valid = wire(&[("a", "one"), ("b", "two")]);
    let control = StorageReadControl::with_limit(1 << 20);
    for end in 0..valid.len() {
        assert!(ScoringParameters::read_image(&mut &valid[..end], &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let mut invalid_utf8 = valid.clone();
    invalid_utf8[24] = 0xff;
    for bytes in [
        invalid_utf8,
        wire(&[("a", "one"), ("a", "duplicate")]),
        wire(&[("b", "two"), ("a", "one")]),
        [valid.as_slice(), &[0]].concat(),
        [b"BADIMAGE".as_slice(), &valid[8..]].concat(),
    ] {
        assert!(ScoringParameters::read_image(&mut bytes.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let denied = StorageReadControl::with_limit(0);
    assert!(matches!(
        ScoringParameters::read_image(&mut valid.as_slice(), &denied),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
    assert!(
        ScoringParameters::read_image(&mut wire(&[]).as_slice(), &denied)
            .unwrap()
            .is_empty()
    );
    control.cancellation().cancel();
    assert!(matches!(
        ScoringParameters::read_image(&mut valid.as_slice(), &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn scoring_parameter_image_native_lease_tracks_every_key_and_value_allocation() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut retained = Vec::new();
    for padding in [0, 1024] {
        let name = format!("signal{}", "x".repeat(padding));
        let value = format!("value{}", "y".repeat(padding));
        let bytes = wire(&[(&name, &value)]);
        let mut decoded = None;
        let measured = allocation_counter::measure(|| {
            decoded = Some(ScoringParameters::read_image(&mut bytes.as_slice(), &control).unwrap());
        });
        retained.push((measured.bytes_current, control.memory().used()));
        let required = control.memory().used();
        let exact = StorageReadControl::with_limit(required);
        let second = ScoringParameters::read_image(&mut bytes.as_slice(), &exact).unwrap();
        let clone = second.clone();
        drop(second);
        assert_eq!(exact.memory().used(), required);
        drop(clone);
        assert_eq!(exact.memory().used(), 0);
        let denied = StorageReadControl::with_limit(required - 1);
        assert!(matches!(
            ScoringParameters::read_image(&mut bytes.as_slice(), &denied),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(denied.memory().used(), 0);
        drop(decoded);
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(
        retained[1].0 - retained[0].0,
        i64::try_from(retained[1].1 - retained[0].1).unwrap()
    );
}

#[test]
fn scoring_parameter_image_output_is_bounded_and_failure_is_terminal() {
    let mut source = ScoringParameters::default();
    source.insert("signal".into(), "x".repeat(4 << 20)).unwrap();
    let capture = StorageReadControl::with_limit(8 << 20);
    let captured = source.capture(&capture).unwrap();
    drop(source);
    let output = StorageReadControl::with_limit(4096);
    let mut part = vec![0; 32768];
    let measured = allocation_counter::measure(|| {
        let mut reader = captured.image_reader(&output).unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let mut total = 0;
        loop {
            let count = reader.read(&mut part).unwrap();
            assert!(count <= 16384);
            if count == 0 {
                break;
            }
            total += count;
        }
        assert_eq!(total, (4 << 20) + 38);
    });
    assert!(measured.bytes_total < 4096, "{measured:?}");
    assert_eq!(output.memory().used(), 0);
    let denied = StorageReadControl::with_limit(0);
    assert!(captured.image_reader(&denied).is_err());
    let mut reader = captured.image_reader(&output).unwrap();
    reader.read(&mut [0]).unwrap();
    output.cancellation().cancel();
    assert!(reader.read(&mut [0]).is_err());
    assert_eq!(output.memory().used(), 0);
    output.cancellation().reset();
    assert!(reader.read(&mut [0]).is_err());
    drop(reader);
    drop(captured);
    assert_eq!(capture.memory().used(), 0);
}

#[test]
fn scoring_parameter_images_release_partial_input_on_io_cancellation_and_unwind() {
    struct Interrupted<'a> {
        bytes: &'a [u8],
        control: &'a StorageReadControl,
        calls: usize,
        mode: u8,
    }
    impl Read for Interrupted<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 8 {
                match self.mode {
                    0 => self.control.cancellation().cancel(),
                    1 => return Err(std::io::Error::other("injected scoring input failure")),
                    _ => panic!("injected scoring input unwind"),
                }
            }
            self.bytes.read(output)
        }
    }
    let bytes = wire(&[("a", "one"), ("b", "two")]);
    for mode in 0..3 {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = Interrupted {
            bytes: &bytes,
            control: &control,
            calls: 0,
            mode,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ScoringParameters::read_image(&mut input, &control)
        }));
        if mode == 2 {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert_eq!(control.memory().used(), 0);
    }
}
