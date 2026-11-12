// SPDX-License-Identifier: MIT
//! End-to-end tests of the code check on hand-written snippets (none of
//! them is taken from git).

use super::*;

const C_SRC: &str = r#"
static int merge_ranges(struct range *r, int nr, int *out_nr)
{
	int i, j = 0;
	if (!nr)
		return 0;
	for (i = 1; i < nr; i++) {
		if (r[i].start <= r[j].end + 1) {
			if (r[i].end > r[j].end)
				r[j].end = r[i].end;
			r[j].count += r[i].count;
		} else {
			j++;
			r[j] = r[i];
		}
	}
	*out_nr = j + 1;
	while (j >= 0 && r[j].count == 0)
		j--;
	return j < 0 ? -1 : r[j].end - r[0].start;
}
"#;

/// The C above, transliterated by hand the way a careless port would look.
const RUST_COPY: &str = r#"
fn merge_ranges(r: &mut [Range], nr: usize, out_nr: &mut i32) -> i64 {
    let mut j = 0;
    if !nr { return 0; }
    for i in 1..nr {
        if r[i].start <= r[j].end + 1 {
            if r[i].end > r[j].end
                r[j].end = r[i].end;
            r[j].count += r[i].count;
        } else {
            j += 1;
            r[j] = r[i];
        }
    }
    *out_nr = j + 1;
    while j >= 0 && r[j].count == 0
        j -= 1;
    return 0;
}
"#;

const RUST_UNRELATED: &str = r#"
fn parse_header(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    Some((key, value.trim().to_string()))
}
"#;

fn corpus(it: &mut Interner) -> Corpus {
    let toks = lex(C_SRC, Lang::C).toks;
    let doc = Doc {
        path: "range.c".into(),
        seq: normalise(&toks, Lang::C, it),
        threshold: None,
    };
    Corpus::new(vec![doc], CODE_PARAMS)
}

#[test]
fn transliterated_copy_is_reported() {
    let mut it = Interner::default();
    let c = corpus(&mut it);
    let seq = normalise(&lex(RUST_COPY, Lang::Rust).toks, Lang::Rust, &mut it);
    let hits = c.find(&seq);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].score >= CODE_PARAMS.threshold, "{hits:?}");
    let lines = line_range(&c.docs[0].seq, hits[0].u);
    assert!(lines.0 >= 7 && lines.1 <= 22, "{lines:?}");
}

#[test]
fn unrelated_code_is_not_reported() {
    let mut it = Interner::default();
    let c = corpus(&mut it);
    let seq = normalise(&lex(RUST_UNRELATED, Lang::Rust).toks, Lang::Rust, &mut it);
    assert!(c.find(&seq).is_empty());
}
