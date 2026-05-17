//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Porter (1980) stemming algorithm.
//!
//! Reference: M. F. Porter, "An Algorithm for Suffix Stripping", *Program*
//! 14(3), 1980. Note that this is the original 1980 algorithm, not Porter2
//! (Snowball English) — they differ on edge cases such as `agreed` and
//! `feedeing`. The output is intentionally identical to the upstream
//! UQA stemmer contract so that BM25 doc frequencies match across
//! engines.

pub fn stem(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() <= 2 {
        return word.to_owned();
    }
    let stemmed = stem_chars(chars);
    stemmed.into_iter().collect()
}

fn stem_chars(mut w: Vec<char>) -> Vec<char> {
    step_1a(&mut w);
    step_1b(&mut w);
    step_1c(&mut w);
    step_2(&mut w);
    step_3(&mut w);
    step_4(&mut w);
    step_5a(&mut w);
    step_5b(&mut w);
    w
}

fn is_consonant(w: &[char], i: usize) -> bool {
    let c = w[i];
    if matches!(c, 'a' | 'e' | 'i' | 'o' | 'u') {
        return false;
    }
    if c == 'y' {
        return i == 0 || !is_consonant(w, i - 1);
    }
    true
}

/// Porter measure m: count of VC sequences in `w[0..=j]`.
fn measure(w: &[char], j: isize) -> usize {
    if j < 0 {
        return 0;
    }
    let j = j as usize;
    let mut n = 0usize;
    let mut i = 0usize;
    loop {
        if i > j {
            return n;
        }
        if !is_consonant(w, i) {
            break;
        }
        i += 1;
    }
    i += 1;
    loop {
        loop {
            if i > j {
                return n;
            }
            if is_consonant(w, i) {
                break;
            }
            i += 1;
        }
        i += 1;
        n += 1;
        loop {
            if i > j {
                return n;
            }
            if !is_consonant(w, i) {
                break;
            }
            i += 1;
        }
        i += 1;
    }
}

fn vowel_in_stem(w: &[char], j: isize) -> bool {
    if j < 0 {
        return false;
    }
    let j = j as usize;
    (0..=j).any(|i| !is_consonant(w, i))
}

fn double_consonant(w: &[char], j: usize) -> bool {
    j >= 1 && w[j] == w[j - 1] && is_consonant(w, j)
}

fn cvc(w: &[char], i: usize) -> bool {
    if i < 2 || !is_consonant(w, i) || is_consonant(w, i - 1) || !is_consonant(w, i - 2) {
        return false;
    }
    !matches!(w[i], 'w' | 'x' | 'y')
}

fn ends_with(w: &[char], suffix: &[char]) -> bool {
    if suffix.len() > w.len() {
        return false;
    }
    let start = w.len() - suffix.len();
    &w[start..] == suffix
}

fn ends_with_str(w: &[char], suffix: &str) -> bool {
    let s: Vec<char> = suffix.chars().collect();
    ends_with(w, &s)
}

fn truncate(w: &mut Vec<char>, n: usize) {
    let new_len = w.len() - n;
    w.truncate(new_len);
}

fn replace_suffix(w: &mut Vec<char>, suffix_len: usize, replacement: &str) {
    truncate(w, suffix_len);
    w.extend(replacement.chars());
}

fn step_1a(w: &mut Vec<char>) {
    if ends_with_str(w, "sses") || ends_with_str(w, "ies") {
        truncate(w, 2);
    } else if !ends_with_str(w, "ss") && ends_with_str(w, "s") {
        truncate(w, 1);
    }
}

fn step_1b(w: &mut Vec<char>) {
    if ends_with_str(w, "eed") {
        let stem_len = w.len() - 3;
        if measure(w, stem_len as isize - 1) > 0 {
            truncate(w, 1);
        }
        return;
    }
    let mut matched = false;
    for suffix in ["ed", "ing"] {
        if ends_with_str(w, suffix)
            && vowel_in_stem(w, w.len() as isize - suffix.len() as isize - 1)
        {
            truncate(w, suffix.len());
            matched = true;
            break;
        }
    }
    if !matched {
        return;
    }
    if ends_with_str(w, "at") || ends_with_str(w, "bl") || ends_with_str(w, "iz") {
        w.push('e');
    } else if double_consonant(w, w.len() - 1) && !matches!(w[w.len() - 1], 'l' | 's' | 'z') {
        truncate(w, 1);
    } else if measure(w, w.len() as isize - 1) == 1 && cvc(w, w.len() - 1) {
        w.push('e');
    }
}

fn step_1c(w: &mut [char]) {
    if ends_with_str(w, "y") && vowel_in_stem(w, w.len() as isize - 2) {
        let last = w.len() - 1;
        w[last] = 'i';
    }
}

fn apply_replacement_table(w: &mut Vec<char>, table: &[(&str, &str)]) {
    for (suffix, replacement) in table {
        if ends_with_str(w, suffix) {
            let stem_len = w.len() - suffix.chars().count();
            if measure(w, stem_len as isize - 1) > 0 {
                replace_suffix(w, suffix.chars().count(), replacement);
            }
            return;
        }
    }
}

fn step_2(w: &mut Vec<char>) {
    apply_replacement_table(
        w,
        &[
            ("ational", "ate"),
            ("tional", "tion"),
            ("enci", "ence"),
            ("anci", "ance"),
            ("izer", "ize"),
            ("abli", "able"),
            ("alli", "al"),
            ("entli", "ent"),
            ("eli", "e"),
            ("ousli", "ous"),
            ("ization", "ize"),
            ("ation", "ate"),
            ("ator", "ate"),
            ("alism", "al"),
            ("iveness", "ive"),
            ("fulness", "ful"),
            ("ousness", "ous"),
            ("aliti", "al"),
            ("iviti", "ive"),
            ("biliti", "ble"),
        ],
    );
}

fn step_3(w: &mut Vec<char>) {
    apply_replacement_table(
        w,
        &[
            ("icate", "ic"),
            ("ative", ""),
            ("alize", "al"),
            ("iciti", "ic"),
            ("ical", "ic"),
            ("ful", ""),
            ("ness", ""),
        ],
    );
}

fn step_4(w: &mut Vec<char>) {
    const SUFFIXES: &[&str] = &[
        "al", "ance", "ence", "er", "ic", "able", "ible", "ant", "ement", "ment", "ent", "ion",
        "ou", "ism", "ate", "iti", "ous", "ive", "ize",
    ];
    for suffix in SUFFIXES {
        if ends_with_str(w, suffix) {
            let stem_len = w.len() - suffix.chars().count();
            if measure(w, stem_len as isize - 1) > 1 {
                if *suffix == "ion" {
                    if stem_len > 0 && matches!(w[stem_len - 1], 's' | 't') {
                        truncate(w, suffix.chars().count());
                    }
                } else {
                    truncate(w, suffix.chars().count());
                }
            }
            return;
        }
    }
}

fn step_5a(w: &mut Vec<char>) {
    if ends_with_str(w, "e") {
        let stem_len = w.len() - 1;
        let m = measure(w, stem_len as isize - 1);
        if m > 1 || (m == 1 && !cvc(w, stem_len - 1)) {
            truncate(w, 1);
        }
    }
}

fn step_5b(w: &mut Vec<char>) {
    let last = w.len().saturating_sub(1);
    if measure(w, last as isize) > 1 && double_consonant(w, last) && w[last] == 'l' {
        truncate(w, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_words_pass_through() {
        assert_eq!(stem("a"), "a");
        assert_eq!(stem("by"), "by");
    }

    #[test]
    fn known_examples() {
        assert_eq!(stem("caresses"), "caress");
        assert_eq!(stem("ponies"), "poni");
        assert_eq!(stem("ties"), "ti");
        assert_eq!(stem("caress"), "caress");
        assert_eq!(stem("cats"), "cat");
        assert_eq!(stem("feed"), "feed");
        assert_eq!(stem("agreed"), "agre");
        assert_eq!(stem("conflated"), "conflat");
        assert_eq!(stem("troubled"), "troubl");
        assert_eq!(stem("happy"), "happi");
        assert_eq!(stem("relational"), "relat");
        assert_eq!(stem("conditional"), "condit");
        assert_eq!(stem("rational"), "ration");
        assert_eq!(stem("triplicate"), "triplic");
        assert_eq!(stem("formative"), "form");
        assert_eq!(stem("electrical"), "electr");
        assert_eq!(stem("hopeful"), "hope");
        assert_eq!(stem("goodness"), "good");
        assert_eq!(stem("revival"), "reviv");
        assert_eq!(stem("homologous"), "homolog");
        assert_eq!(stem("controll"), "control");
    }
}
