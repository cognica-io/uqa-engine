//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native expression identity retains its exact preorder encoding across checkpoints.

use regex_syntax::hir::{Class, Hir, HirKind, Look};
use sha2::{Digest, Sha256};

use crate::{work::Work, AnalysisError, AnalysisResult};

pub(super) async fn expression(
    pattern: &str,
    component: &'static str,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<String> {
    work.checkpoint().await?;
    let hir = regex_syntax::Parser::new()
        .parse(pattern)
        .map_err(|source| AnalysisError::InvalidRegex {
            component,
            pattern: pattern.into(),
            source: regex::Error::Syntax(source.to_string()),
        })?;
    let mut hash = Sha256::new();
    hash.update(b"UQA regex expression\0v1\0");
    let unicode_word = hash_hir(&hir, &mut hash, work).await?;
    hash.update([u8::from(unicode_word)]);
    if unicode_word {
        work.checkpoint().await?;
        let unicode_class = regex_syntax::Parser::new()
            .parse("\\w")
            .map_err(|_| super::super::invalid("Unicode word profile is unavailable"))?;
        hash_hir(&unicode_class, &mut hash, work).await?;
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) async fn property_expression(
    property: icu_properties::CodePointSetDataBorrowed<'_>,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<String> {
    // The encoding is the same Unicode class as the native regular-expression profile.
    let mut count = 0;
    for _ in property.iter_ranges() {
        work.records(count).await?;
        count += 1;
    }
    let mut hash = Sha256::new();
    hash.update(b"UQA regex expression\0v1\0");
    hash.update([2]);
    length(&mut hash, count);
    for (index, range) in property.iter_ranges().enumerate() {
        work.records(index).await?;
        hash.update(range.start().to_le_bytes());
        hash.update(range.end().to_le_bytes());
    }
    hash.update([0]);
    Ok(format!("{:x}", hash.finalize()))
}

fn length(hash: &mut Sha256, length: usize) {
    hash.update((length as u64).to_le_bytes());
}

enum Frame<'a> {
    Node(&'a Hir),
    Children(std::slice::Iter<'a, Hir>),
}

pub(super) async fn hash_hir(
    root: &Hir,
    hash: &mut Sha256,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<bool> {
    let mut pending = vec![Frame::Node(root)];
    let mut unicode_word = false;
    while let Some(frame) = pending.pop() {
        work.checkpoint().await?;
        let node = match frame {
            Frame::Node(node) => node,
            Frame::Children(mut children) => {
                if let Some(child) = children.next() {
                    pending.push(Frame::Children(children));
                    pending.push(Frame::Node(child));
                }
                continue;
            }
        };
        match node.kind() {
            HirKind::Empty => hash.update([0]),
            HirKind::Literal(literal) => {
                hash.update([1]);
                length(hash, literal.0.len());
                bytes(&literal.0, hash, work).await?;
            }
            HirKind::Class(Class::Unicode(class)) => {
                hash.update([2]);
                length(hash, class.ranges().len());
                for (index, range) in class.ranges().iter().enumerate() {
                    work.records(index).await?;
                    hash.update((range.start() as u32).to_le_bytes());
                    hash.update((range.end() as u32).to_le_bytes());
                }
            }
            HirKind::Class(Class::Bytes(class)) => {
                hash.update([3]);
                length(hash, class.ranges().len());
                for (index, range) in class.ranges().iter().enumerate() {
                    work.records(index).await?;
                    hash.update([range.start(), range.end()]);
                }
            }
            HirKind::Look(look) => {
                hash.update([4]);
                hash.update(look.as_repr().to_le_bytes());
                unicode_word |= matches!(
                    look,
                    Look::WordUnicode
                        | Look::WordUnicodeNegate
                        | Look::WordStartUnicode
                        | Look::WordEndUnicode
                        | Look::WordStartHalfUnicode
                        | Look::WordEndHalfUnicode
                );
            }
            HirKind::Repetition(repetition) => {
                hash.update([5]);
                hash.update(repetition.min.to_le_bytes());
                hash.update([
                    u8::from(repetition.max.is_some()),
                    u8::from(repetition.greedy),
                ]);
                hash.update(repetition.max.unwrap_or(0).to_le_bytes());
                pending.push(Frame::Node(&repetition.sub));
            }
            HirKind::Capture(capture) => {
                hash.update([6]);
                hash.update(capture.index.to_le_bytes());
                hash.update([u8::from(capture.name.is_some())]);
                if let Some(name) = &capture.name {
                    length(hash, name.len());
                    bytes(name.as_bytes(), hash, work).await?;
                }
                pending.push(Frame::Node(&capture.sub));
            }
            HirKind::Concat(children) | HirKind::Alternation(children) => {
                hash.update([if matches!(node.kind(), HirKind::Concat(_)) {
                    7
                } else {
                    8
                }]);
                length(hash, children.len());
                pending.push(Frame::Children(children.iter()));
            }
        }
    }
    Ok(unicode_word)
}

async fn bytes(
    value: &[u8],
    hash: &mut Sha256,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<()> {
    for fragment in value.chunks(4096) {
        work.checkpoint().await?;
        hash.update(fragment);
    }
    Ok(())
}
