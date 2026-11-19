//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn decode(
    kind: usize,
    input: &mut dyn Read,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> Result<(), SQLError> {
    if kind == 0 {
        drop(BuiltinRoutineSecurities::read_image(input, memory, token)?);
    } else {
        drop(SystemRelationSecurities::read_image(input, memory, token)?);
    }
    Ok(())
}

#[test]
fn authority_image_import_rejects_corrupt_framing_and_native_keys() {
    let token = CancellationToken::new();
    for (kind, image) in images().iter().enumerate() {
        for length in 0..=image.len() {
            let mut bad = image[..length].to_vec();
            if length == image.len() {
                bad.push(0);
            }
            let memory = MemoryBudget::new(1 << 20);
            assert!(
                decode(kind, &mut bad.as_slice(), &memory, &token).is_err(),
                "{kind} {length}"
            );
            assert_eq!(memory.used(), 0);
        }
        let mut mutations = vec![(0, vec![0])];
        if kind == 0 {
            mutations.extend([
                (16, vec![0; 4]),                      // OID
                (20, vec![0; 16]),                     // tuple revision
                (44, vec![2]),                         // grantee option
                (45, vec![0; 8]),                      // grantor OID
                (69, vec![2]),                         // grant option
                (36, u64::MAX.to_le_bytes().to_vec()), // impossible ACL length
            ]);
        } else {
            mutations.extend([
                (24, vec![b'x']),                      // unknown system namespace
                (50, u64::MAX.to_le_bytes().to_vec()), // impossible column count
                (58, vec![2]),                         // table option
                (59, vec![0; 16]),                     // table revision
            ]);
            let column = image.windows(7).position(|x| x == b"relname").unwrap();
            mutations.push((column, b"unknown".to_vec()));
            mutations.push((column + 7, vec![0; 16]));
        }
        for (offset, bytes) in mutations {
            let mut bad = image.clone();
            bad[offset..offset + bytes.len()].copy_from_slice(&bytes);
            let memory = MemoryBudget::new(1 << 20);
            assert!(
                decode(kind, &mut bad.as_slice(), &memory, &token).is_err(),
                "{kind} offset {offset}"
            );
            assert_eq!(memory.used(), 0);
        }
    }
    // Duplicating all records violates native key order even though both copies are individually well formed.
    for (kind, image) in images().iter().enumerate() {
        let count = u64::from_le_bytes(image[8..16].try_into().unwrap());
        let mut duplicate = image.clone();
        duplicate[8..16].copy_from_slice(&(2 * count).to_le_bytes());
        duplicate.extend_from_slice(&image[16..]);
        let memory = MemoryBudget::new(1 << 20);
        assert_eq!(
            decode(kind, &mut duplicate.as_slice(), &memory, &token)
                .unwrap_err()
                .sqlstate(),
            Some("XX001")
        );
        assert_eq!(memory.used(), 0);
    }
    reject_column_order(&token);
}

fn reject_column_order(token: &CancellationToken) {
    let mut system = SystemRelationSecurities::new();
    system
        .insert(
            RelationIdentity::new("pg_catalog", "pg_class"),
            SystemRelationSecurity {
                table: None,
                columns: ["relname", "relnamespace"]
                    .into_iter()
                    .map(|name| {
                        (
                            name.into(),
                            SystemAcl {
                                revision: [1; 16],
                                acl: vec![],
                            },
                        )
                    })
                    .collect(),
            },
        )
        .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&memory, token, token);
    let saved = system.capture(&control).unwrap();
    let image = streamed(saved.image_reader(token).unwrap(), 1);
    assert_eq!(image.len(), 142);
    assert_eq!(&image[67..74], b"relname");
    assert_eq!(&image[106..118], b"relnamespace");
    for duplicate in [false, true] {
        let mut bad = image[..59].to_vec();
        bad.extend_from_slice(if duplicate {
            &image[59..98]
        } else {
            &image[98..]
        });
        bad.extend_from_slice(&image[59..98]);
        let memory = MemoryBudget::new(1 << 20);
        assert_eq!(
            decode(1, &mut bad.as_slice(), &memory, token)
                .unwrap_err()
                .sqlstate(),
            Some("XX001")
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn authority_image_import_retains_admission_through_shared_generations() {
    let token = CancellationToken::new();
    let capture_memory = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&capture_memory, &token, &token);
    for (kind, image) in images().iter().enumerate() {
        let memory = MemoryBudget::new(1 << 20);
        let retained = if kind == 0 {
            let value =
                BuiltinRoutineSecurities::read_image(&mut image.as_slice(), &memory, &token)
                    .unwrap();
            let retained = memory.used();
            drop(value);
            retained
        } else {
            let value =
                SystemRelationSecurities::read_image(&mut image.as_slice(), &memory, &token)
                    .unwrap();
            let retained = memory.used();
            drop(value);
            retained
        };
        let required = memory.peak();
        assert_eq!(memory.used(), 0);
        // Temporary column-name production can adapt vector growth to its allowance; the retained native footprint is mandatory.
        for limit in [0, 16, retained - 1] {
            let memory = MemoryBudget::new(limit);
            assert_eq!(
                decode(kind, &mut image.as_slice(), &memory, &token)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
            assert_eq!(memory.used(), 0);
        }
        let memory = MemoryBudget::new(required);
        let saved: Box<dyn AuthorityImage> = if kind == 0 {
            let mut restored =
                BuiltinRoutineSecurities::read_image(&mut image.as_slice(), &memory, &token)
                    .unwrap();
            let clone = restored.clone();
            let captured = restored.capture(&control).unwrap();
            assert_eq!(memory.used(), capture_memory.used());
            restored.clear();
            drop((restored, clone));
            Box::new(captured)
        } else {
            let mut restored =
                SystemRelationSecurities::read_image(&mut image.as_slice(), &memory, &token)
                    .unwrap();
            let clone = restored.clone();
            let captured = restored.capture(&control).unwrap();
            assert_eq!(memory.used(), capture_memory.used());
            restored.clear();
            drop((restored, clone));
            Box::new(captured)
        };
        assert!(memory.used() > 0);
        assert_eq!(streamed(saved.reader(&token), 3), *image);
        drop(saved);
        assert_eq!(memory.used(), 0);
        assert_eq!(capture_memory.used(), 0);
    }
}

#[test]
fn authority_image_input_cancellation_releases_partial_native_state() {
    struct CancelInput<'a> {
        bytes: &'a [u8],
        token: &'a CancellationToken,
        left: usize,
    }
    impl Read for CancelInput<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.left == 0 {
                self.token.cancel();
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let length = self.bytes.len().min(out.len()).min(self.left).min(3);
            out[..length].copy_from_slice(&self.bytes[..length]);
            self.left -= length;
            self.bytes = &self.bytes[length..];
            Ok(length)
        }
    }
    for (kind, image) in images().iter().enumerate() {
        for left in [0, 17, 57, image.len() - 1] {
            let token = CancellationToken::new();
            let memory = MemoryBudget::new(1 << 20);
            let mut reader = CancelInput {
                bytes: image,
                token: &token,
                left,
            };
            assert_eq!(
                decode(kind, &mut reader, &memory, &token)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), 0);
        }
    }
}
