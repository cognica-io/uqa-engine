//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact descriptor dependencies retain their verified encoded bytes with the compiled revision.

use super::CompiledAnalyzer;

mod capture;
pub use capture::CapturedAnalyzer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnalyzerResourceKind {
    Nori,
    Kuromoji,
}

/// A borrowed immutable dictionary artifact, identified independently of names and source paths.
#[derive(Clone, Copy)]
pub struct AnalyzerResource<'a> {
    kind: AnalyzerResourceKind,
    sha256: [u8; 32],
    bytes: &'a [u8],
}

impl<'a> AnalyzerResource<'a> {
    pub fn kind(self) -> AnalyzerResourceKind {
        self.kind
    }

    pub fn sha256(self) -> [u8; 32] {
        self.sha256
    }

    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

impl CompiledAnalyzer {
    /// Borrow every dictionary needed by this exact descriptor, ordered by language and artifact hash, without consulting a resolver or cache. Inline user rules, synonyms and expanded stop sets remain in the descriptor itself.
    pub fn dictionary_resources(&self) -> impl Iterator<Item = AnalyzerResource<'_>> {
        #[cfg(feature = "nori")]
        let korean = self
            .nori_dictionaries
            .iter()
            .map(|dictionary| AnalyzerResource {
                kind: AnalyzerResourceKind::Nori,
                sha256: dictionary.sha256().into_bytes(),
                bytes: dictionary.bytes(),
            });
        #[cfg(not(feature = "nori"))]
        let korean = std::iter::empty();
        #[cfg(feature = "kuromoji")]
        let japanese = self
            .kuromoji_dictionaries
            .iter()
            .map(|dictionary| AnalyzerResource {
                kind: AnalyzerResourceKind::Kuromoji,
                sha256: dictionary.sha256().into_bytes(),
                bytes: dictionary.bytes(),
            });
        #[cfg(not(feature = "kuromoji"))]
        let japanese = std::iter::empty();
        korean.chain(japanese)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ordinary_compiled_analyzers_have_no_external_dictionary_dependencies() {
        let compiled = crate::standard_analyzer("english").compile().unwrap();
        assert_eq!(compiled.dictionary_resources().count(), 0);
        assert_eq!(compiled.analyze("The cats").unwrap(), ["cat"]);
    }
}
