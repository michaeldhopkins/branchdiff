#![no_main]

//! ROUNDTRIP + NEVER-PANICS target: the `git cat-file --batch` stream reader.
//!
//! branchdiff fetches every file's base/HEAD/index content through one
//! `git cat-file --batch` process per ref and reads the responses back in request
//! order. The stream is length-framed, so one misread header desynchronises every
//! response after it, and the wrong content is then shown under the wrong path
//! with nothing to say so.
//!
//! First input byte picks the mode:
//!
//! - **Raw** (even): the rest is the stream itself, read against four fixed paths.
//!   Must not panic or allocate what the stream merely claims (a header's size
//!   field is a number anyone can write), and may only report requested paths.
//! - **Roundtrip** (odd): build a well-formed stream from a fuzzed list of
//!   responses (blob, tree, or an error line), exactly as git formats them, and
//!   assert the reader returns exactly the blobs. Paths are fuzzed, since an error
//!   response echoes the path back and a path can contain spaces and digits. Paths
//!   containing `\n` are dropped: git reads its requests line by line, so such a
//!   path cannot be requested at all (a frame limit, not a reader bug).

use std::collections::HashMap;

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
enum Response {
    Blob(Vec<u8>),
    Tree(Vec<u8>),
    Missing,
    Ambiguous,
}

#[derive(Arbitrary, Debug)]
struct Entry {
    git_ref: RefKind,
    path: String,
    response: Response,
}

#[derive(Arbitrary, Debug, Clone, Copy)]
enum RefKind {
    Head,
    Index,
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn raw(stream: &[u8]) {
    let paths = ["a", "b", "c", "d"];
    let got = branchdiff::fuzz_api::read_cat_file_batch(stream, &paths);
    for key in got.keys() {
        assert!(paths.contains(&key.as_str()), "reported a path nobody asked for: {key:?}");
    }
}

fn roundtrip(entries: &[Entry]) {
    let mut stream = Vec::new();
    let mut expected: HashMap<String, String> = HashMap::new();
    let mut paths: Vec<&str> = Vec::new();

    for e in entries {
        let spec = match e.git_ref {
            RefKind::Head => format!("HEAD:{}", e.path),
            RefKind::Index => format!(":{}", e.path),
        };
        match &e.response {
            Response::Blob(content) => {
                stream.extend_from_slice(format!("{SHA} blob {}\n", content.len()).as_bytes());
                stream.extend_from_slice(content);
                stream.push(b'\n');
                expected.insert(e.path.clone(), String::from_utf8_lossy(content).into_owned());
            }
            Response::Tree(content) => {
                stream.extend_from_slice(format!("{SHA} tree {}\n", content.len()).as_bytes());
                stream.extend_from_slice(content);
                stream.push(b'\n');
            }
            Response::Missing => stream.extend_from_slice(format!("{spec} missing\n").as_bytes()),
            Response::Ambiguous => stream.extend_from_slice(format!("{spec} ambiguous\n").as_bytes()),
        }
        paths.push(&e.path);
    }

    let got = branchdiff::fuzz_api::read_cat_file_batch(&stream, &paths);
    assert_eq!(got, expected, "stream read back differently from how it was written");
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else { return };
    if mode % 2 == 0 {
        raw(rest);
        return;
    }
    let Ok(entries) = Vec::<Entry>::arbitrary_take_rest(Unstructured::new(rest)) else { return };
    let entries: Vec<Entry> = entries.into_iter().filter(|e| !e.path.contains('\n')).collect();
    roundtrip(&entries);
});
