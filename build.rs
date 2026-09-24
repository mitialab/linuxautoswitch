use fst::SetBuilder;
use std::collections::BTreeSet;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter};
use std::path::Path;

fn build_fst(src: &str, dst: &Path) {
    println!("cargo:rerun-if-changed={src}");

    let file = File::open(src).unwrap_or_else(|e| panic!("failed to open {src}: {e}"));
    let reader = BufReader::new(file);

    // Read into a sorted, deduplicated set first: `fst::SetBuilder` requires
    // strictly increasing keys, and this keeps the build robust even if the
    // source wordlist is ever edited out of order.
    let mut words = BTreeSet::new();
    for line in reader.lines() {
        let line = line.unwrap_or_else(|e| panic!("failed to read {src}: {e}"));
        let word = line.trim();
        if !word.is_empty() {
            words.insert(word.to_string());
        }
    }

    let out = File::create(dst).unwrap_or_else(|e| panic!("failed to create {dst:?}: {e}"));
    let mut builder = SetBuilder::new(BufWriter::new(out))
        .unwrap_or_else(|e| panic!("failed to start fst builder for {dst:?}: {e}"));
    for word in &words {
        builder
            .insert(word)
            .unwrap_or_else(|e| panic!("failed to insert {word:?} into {dst:?}: {e}"));
    }
    builder
        .finish()
        .unwrap_or_else(|e| panic!("failed to finish fst builder for {dst:?}: {e}"));
}

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    build_fst(
        "assets/dictionaries/en_raw.txt",
        &Path::new(&out_dir).join("en.fst"),
    );
    build_fst(
        "assets/dictionaries/ru_raw.txt",
        &Path::new(&out_dir).join("ru.fst"),
    );
}
