use sha2::{Digest, Sha256};
use uqa_analysis::{Analyzer, Tokenizer, TokenFilter};
fn main() {
    assert_eq!(char::UNICODE_VERSION, (17, 0, 0));
    let output = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    std::fs::create_dir_all(&output).unwrap();
    for (name, tokenizer) in [("keyword", Tokenizer::Keyword), ("whitespace", Tokenizer::Whitespace), ("standard", Tokenizer::Standard), ("ngram", Tokenizer::NGram { min_gram: 1, max_gram: 2 })] {
        let compiled = Analyzer::new(tokenizer, vec![TokenFilter::Lowercase], vec![]).compile().unwrap();
        std::fs::write(output.join(format!("{name}.json")), compiled.descriptor().canonical_json()).unwrap();
    }
    let standard = uqa_analysis::standard_analyzer("english").compile().unwrap();
    std::fs::write(output.join("english.json"), standard.descriptor().canonical_json()).unwrap();
    let compiled = Analyzer::new(Tokenizer::Keyword, vec![TokenFilter::Lowercase], vec![]).compile().unwrap();
    let mut hash = Sha256::new();
    let mut count = 0;
    for scalar in 0..=0x10ffffu32 {
        let Some(c) = char::from_u32(scalar) else { continue };
        hash.update(scalar.to_le_bytes());
        hash.update([u8::from(c.is_whitespace())]);
        for (prefix, suffix) in [("A", "Σ"), ("AΣ", ""), ("AΣ", "A")] {
            let input = format!("{prefix}{c}{suffix}");
            let terms = compiled.analyze(&input).unwrap();
            assert_eq!(terms.len(), 1);
            hash.update((terms[0].len() as u64).to_le_bytes());
            hash.update(terms[0].as_bytes());
            count += 1;
        }
    }
    println!("cases={count} sha256={:x}", hash.finalize());
}
