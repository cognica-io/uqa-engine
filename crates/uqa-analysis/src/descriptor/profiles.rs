//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fingerprints describe the Unicode behavior of the components actually present.

use regex_syntax::hir::{Class, Hir, HirKind, Look};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{AnalysisError, AnalysisResult, Analyzer, CharFilter, TokenFilter, Tokenizer};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeProfiles {
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub rust_unicode: Option<(u8, u8, u8)>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub normalization_unicode: Option<(u8, u8, u8)>,
    pub expressions: Vec<String>,
}

impl RuntimeProfiles {
    pub fn resolve(config: &Analyzer) -> AnalysisResult<Self> {
        let mut expressions = Vec::new();
        for filter in &config.char_filters {
            let pattern = match filter {
                CharFilter::HTMLStrip => "<[^>]+>",
                CharFilter::PatternReplace { pattern, .. } => pattern,
                CharFilter::Mapping { .. } => continue,
            };
            expressions.push(expression(pattern, "pattern-replace character filter")?);
        }
        if matches!(config.tokenizer, Tokenizer::NGram { .. }) {
            config.tokenizer.validate()?;
        }
        let (pattern, rust_tokenizer) = match &config.tokenizer {
            Tokenizer::Whitespace | Tokenizer::NGram { .. } => (None, true),
            Tokenizer::Standard => (Some("\\w+"), false),
            Tokenizer::Letter => (Some("[a-zA-Z]+"), false),
            Tokenizer::Pattern { pattern } => (Some(pattern.as_str()), false),
            Tokenizer::Keyword => (None, false),
            #[cfg(feature = "nori")]
            Tokenizer::Nori(_) => (None, false),
        };
        if let Some(pattern) = pattern {
            expressions.push(expression(pattern, "pattern tokenizer")?);
        }
        let rust_lower = config
            .token_filters
            .iter()
            .any(|filter| matches!(filter, TokenFilter::Lowercase));
        if rust_lower {
            let context = [
                expression(r"\p{Cased}", "lowercase cased property")?,
                expression(r"\p{Case_Ignorable}", "lowercase ignorable property")?,
            ];
            extend_lowercase_context(&mut expressions, char::UNICODE_VERSION, context);
        }
        let normalization = config
            .token_filters
            .iter()
            .any(|filter| matches!(filter, TokenFilter::ASCIIFolding));
        Ok(Self {
            rust_unicode: (rust_tokenizer || rust_lower).then_some(char::UNICODE_VERSION),
            normalization_unicode: normalization.then_some(unicode_normalization::UNICODE_VERSION),
            expressions,
        })
    }
}

// These exact context tables already belong to the Rust Unicode 16 profile. Scalar and contextual differentials verify their equivalence to str::to_lowercase. Any other version or table content must contribute its own expression identity.
const RUST_UNICODE_16_CONTEXT: [&str; 2] = [
    "0609b22ae1ba741f6ae6cf515ce9ba758a6ebcf7fe80b2d2e7726b25750c2f62",
    "5af2b098843fc4e94e45a5bde9e52611f461730f85ddedca2ab9fce2f7727a82",
];

fn extend_lowercase_context(
    expressions: &mut Vec<String>,
    rust_unicode: (u8, u8, u8),
    context: [String; 2],
) {
    if rust_unicode != (16, 0, 0) || context != RUST_UNICODE_16_CONTEXT {
        expressions.extend(context);
    }
}

fn expression(pattern: &str, component: &'static str) -> AnalysisResult<String> {
    let hir = regex_syntax::Parser::new()
        .parse(pattern)
        .map_err(|source| AnalysisError::InvalidRegex {
            component,
            pattern: pattern.into(),
            source: regex::Error::Syntax(source.to_string()),
        })?;
    let mut hash = Sha256::new();
    hash.update(b"UQA regex expression\0v1\0");
    let unicode_word = hash_hir(&hir, &mut hash);
    hash.update([u8::from(unicode_word)]);
    if unicode_word {
        let word = regex_syntax::Parser::new()
            .parse("\\w")
            .map_err(|_| super::invalid("Unicode word profile is unavailable"))?;
        hash_hir(&word, &mut hash);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn length(hash: &mut Sha256, length: usize) {
    hash.update((length as u64).to_le_bytes());
}

fn hash_hir(root: &Hir, hash: &mut Sha256) -> bool {
    let mut pending = vec![root];
    let mut unicode_word = false;
    while let Some(node) = pending.pop() {
        match node.kind() {
            HirKind::Empty => hash.update([0]),
            HirKind::Literal(literal) => {
                hash.update([1]);
                length(hash, literal.0.len());
                hash.update(&literal.0);
            }
            HirKind::Class(Class::Unicode(class)) => {
                hash.update([2]);
                length(hash, class.ranges().len());
                for range in class.ranges() {
                    hash.update((range.start() as u32).to_le_bytes());
                    hash.update((range.end() as u32).to_le_bytes());
                }
            }
            HirKind::Class(Class::Bytes(class)) => {
                hash.update([3]);
                length(hash, class.ranges().len());
                for range in class.ranges() {
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
                pending.push(&repetition.sub);
            }
            HirKind::Capture(capture) => {
                hash.update([6]);
                hash.update(capture.index.to_le_bytes());
                hash.update([u8::from(capture.name.is_some())]);
                if let Some(name) = &capture.name {
                    length(hash, name.len());
                    hash.update(name.as_bytes());
                }
                pending.push(&capture.sub);
            }
            HirKind::Concat(children) | HirKind::Alternation(children) => {
                hash.update([if matches!(node.kind(), HirKind::Concat(_)) {
                    7
                } else {
                    8
                }]);
                length(hash, children.len());
                pending.extend(children.iter().rev());
            }
        }
    }
    unicode_word
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_lowercase_context_tables_cannot_reuse_an_implicit_rust_profile() {
        let known = RUST_UNICODE_16_CONTEXT.map(str::to_owned);
        let mut expressions = vec!["preceding tokenizer".to_owned()];
        extend_lowercase_context(&mut expressions, (16, 0, 0), known.clone());
        assert_eq!(expressions, ["preceding tokenizer"]);
        for (version, context) in [
            ((17, 0, 0), known.clone()),
            (
                (16, 0, 0),
                [
                    expression("[A-Z]", "changed cased").unwrap(),
                    known[1].clone(),
                ],
            ),
            (
                (16, 0, 0),
                [
                    known[0].clone(),
                    expression("['.]", "changed ignorable").unwrap(),
                ],
            ),
        ] {
            let mut expressions = vec!["preceding tokenizer".to_owned()];
            extend_lowercase_context(&mut expressions, version, context.clone());
            assert_eq!(expressions[0], "preceding tokenizer");
            assert_eq!(&expressions[1..], context);
        }
    }

    #[test]
    fn expression_identity_preserves_structure_and_uses_fixed_width_encoding() {
        let profile = |pattern| expression(pattern, "test pattern").unwrap();
        assert_eq!(
            profile("hello"),
            "19646533c4da6bb1fe7a2e586f74910409bbf2508139b3c8e4697dd75fc3704a"
        );
        for (left, right) in [
            ("", "a"),
            ("ab", "ba"),
            ("a+", "a+?"),
            ("a{1,2}", "a{1,3}"),
            ("a{1,}", "a{2,}"),
            ("(a)", "(?P<n>a)"),
            ("(?P<a>x)", "(?P<b>x)"),
            ("^a", "a$"),
            ("aa|ab", "ab|aa"),
            ("[ab]", "[ac]"),
            ("[ab]", "(?-u:[ab])"),
            ("\\w", "(?-u:\\w)"),
            ("\\b", "(?-u:\\b)"),
        ] {
            assert_ne!(profile(left), profile(right), "{left} / {right}");
        }
    }

    #[test]
    fn all_unicode_word_look_types_require_the_expanded_word_table() {
        for pattern in [
            "\\b",
            "\\B",
            "\\b{start}",
            "\\b{end}",
            "\\b{start-half}",
            "\\b{end-half}",
        ] {
            let hir = regex_syntax::Parser::new().parse(pattern).unwrap();
            assert!(hash_hir(&hir, &mut Sha256::new()), "{pattern}");
            let ascii = regex_syntax::Parser::new()
                .parse(&format!("(?-u:{pattern})"))
                .unwrap();
            assert!(!hash_hir(&ascii, &mut Sha256::new()), "{pattern}");
        }
    }
}
