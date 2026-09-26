use fst::SetBuilder;
use std::borrow::Cow;
use std::env;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

fn build_fst(sources: &[&str], dst: &Path) {
    let texts: Vec<String> = sources
        .iter()
        .map(|src| {
            println!("cargo:rerun-if-changed={src}");
            fs::read_to_string(src).unwrap_or_else(|e| panic!("failed to read {src}: {e}"))
        })
        .collect();

    // Sort and deduplicate first: `fst::SetBuilder` requires strictly
    // increasing keys, and this keeps the build robust even if a source
    // wordlist is ever edited out of order. Borrowing lines from `texts`
    // avoids a heap allocation per word, and sorting an already-sorted list
    // is close to linear.
    let mut words: Vec<Cow<str>> = Vec::new();
    for text in &texts {
        for word in text.lines().map(str::trim).filter(|w| !w.is_empty()) {
            // Most people type Russian `ё` as `е`, so accept that spelling too.
            if word.contains('ё') {
                words.push(Cow::Owned(word.replace('ё', "е")));
            }
            words.push(Cow::Borrowed(word));
        }
    }
    words.sort_unstable();
    words.dedup();

    let out = File::create(dst).unwrap_or_else(|e| panic!("failed to create {dst:?}: {e}"));
    let mut builder = SetBuilder::new(BufWriter::new(out))
        .unwrap_or_else(|e| panic!("failed to start fst builder for {dst:?}: {e}"));
    for word in &words {
        builder
            .insert(word.as_bytes())
            .unwrap_or_else(|e| panic!("failed to insert {word:?} into {dst:?}: {e}"));
    }
    builder
        .finish()
        .unwrap_or_else(|e| panic!("failed to finish fst builder for {dst:?}: {e}"));
}

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    build_fst(
        &[
            "assets/dictionaries/en_raw.txt",
            "assets/dictionaries/en_extra.txt",
        ],
        &Path::new(&out_dir).join("en.fst"),
    );
    build_fst(
        &[
            "assets/dictionaries/ru_raw.txt",
            "assets/dictionaries/ru_extra.txt",
        ],
        &Path::new(&out_dir).join("ru.fst"),
    );
}
