//! A short hash that changes when the diff would: the cheap answer to "has it
//! changed?" for a tool that polls.
//!
//! The VCS backends gather the inputs without snapshotting (`Vcs::fingerprint_inputs`);
//! this module only combines them, so it is a pure function of its arguments.

/// What a backend reports about the comparison, gathered without a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FingerprintInputs {
    /// Commit id of the base the diff is taken from.
    pub base: String,
    /// Commit id of the head (and of the stack tip above it, if any).
    pub head: String,
    /// Backend-specific working state that is not file content, such as git's
    /// index (staged against unstaged changes the diff colours differently).
    pub state: String,
    /// Paths that differ between the base and the working copy.
    pub changed: Vec<String>,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over bytes. Written out rather than taken from `DefaultHasher`,
/// whose algorithm is unspecified and may differ between Rust releases: the
/// value is compared across runs of different builds.
fn fnv(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Hash a field with its length in front, so that moving a byte from one field
/// to the next changes the result.
fn field(hash: u64, bytes: &[u8]) -> u64 {
    fnv(fnv(hash, &(bytes.len() as u64).to_le_bytes()), bytes)
}

/// The fingerprint: 16 lowercase hex digits.
///
/// `read` returns a changed path's current content, or `None` when it is
/// absent (a deletion is part of the state, and differs from an empty file).
/// The order of `changed` does not matter.
pub fn fingerprint(inputs: &FingerprintInputs, read: impl Fn(&str) -> Option<Vec<u8>>) -> String {
    let mut hash = FNV_OFFSET;
    hash = field(hash, inputs.base.as_bytes());
    hash = field(hash, inputs.head.as_bytes());
    hash = field(hash, inputs.state.as_bytes());

    let mut paths: Vec<&String> = inputs.changed.iter().collect();
    paths.sort();
    paths.dedup();
    hash = fnv(hash, &(paths.len() as u64).to_le_bytes());
    for path in paths {
        hash = field(hash, path.as_bytes());
        match read(path) {
            Some(content) => {
                hash = fnv(hash, &[1]);
                hash = field(hash, &content);
            }
            None => hash = fnv(hash, &[0]),
        }
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn inputs(base: &str, head: &str, state: &str, changed: &[&str]) -> FingerprintInputs {
        FingerprintInputs {
            base: base.into(),
            head: head.into(),
            state: state.into(),
            changed: changed.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn files(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<Vec<u8>> {
        let map: HashMap<String, Vec<u8>> =
            pairs.iter().map(|(p, c)| ((*p).to_string(), c.as_bytes().to_vec())).collect();
        move |p| map.get(p).cloned()
    }

    #[test]
    fn it_is_sixteen_lowercase_hex_digits() {
        let f = fingerprint(&inputs("b", "h", "", &[]), files(&[]));
        assert_eq!(f.len(), 16);
        assert!(f.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)), "{f}");
    }

    /// Pinned: the value is compared across builds, so it must never drift.
    #[test]
    fn the_value_for_known_inputs_is_stable() {
        let f = fingerprint(&inputs("base1", "head1", "M a.rs", &["a.rs"]), files(&[("a.rs", "x\n")]));
        assert_eq!(f, "50223c162af14d23");
    }

    #[test]
    fn editing_a_changed_file_changes_it() {
        let i = inputs("b", "h", "", &["a.rs"]);
        assert_ne!(
            fingerprint(&i, files(&[("a.rs", "one")])),
            fingerprint(&i, files(&[("a.rs", "two")]))
        );
    }

    #[test]
    fn deleting_a_changed_file_differs_from_emptying_it() {
        let i = inputs("b", "h", "", &["a.rs"]);
        assert_ne!(fingerprint(&i, files(&[])), fingerprint(&i, files(&[("a.rs", "")])));
    }

    #[test]
    fn a_new_commit_or_base_changes_it() {
        let read = files(&[]);
        let a = fingerprint(&inputs("b", "h1", "", &[]), &read);
        assert_ne!(a, fingerprint(&inputs("b", "h2", "", &[]), &read));
        assert_ne!(a, fingerprint(&inputs("b2", "h1", "", &[]), &read));
        assert_ne!(a, fingerprint(&inputs("b", "h1", "staged", &[]), &read));
    }

    #[test]
    fn the_order_of_changed_paths_does_not_matter() {
        let read = files(&[("a", "1"), ("b", "2")]);
        assert_eq!(
            fingerprint(&inputs("b", "h", "", &["a", "b"]), &read),
            fingerprint(&inputs("b", "h", "", &["b", "a"]), &read)
        );
    }

    #[test]
    fn moving_a_byte_between_fields_changes_it() {
        let read = files(&[]);
        assert_ne!(
            fingerprint(&inputs("ab", "c", "", &[]), &read),
            fingerprint(&inputs("a", "bc", "", &[]), &read)
        );
    }

    proptest! {
        #[test]
        fn it_is_deterministic(base in ".{0,12}", head in ".{0,12}", state in ".{0,12}",
                               paths in proptest::collection::vec("[a-z/.]{1,8}", 0..5),
                               content in ".{0,20}") {
            let i = FingerprintInputs { base, head, state, changed: paths };
            let read = |_: &str| Some(content.as_bytes().to_vec());
            prop_assert_eq!(fingerprint(&i, read), fingerprint(&i, read));
        }

        #[test]
        fn permuting_the_changed_paths_never_changes_it(
            mut paths in proptest::collection::vec("[a-z]{1,6}", 0..6),
            content in ".{0,10}",
        ) {
            let read = |p: &str| Some(format!("{p}{content}").into_bytes());
            let forward = FingerprintInputs { changed: paths.clone(), ..Default::default() };
            paths.reverse();
            let backward = FingerprintInputs { changed: paths, ..Default::default() };
            prop_assert_eq!(fingerprint(&forward, read), fingerprint(&backward, read));
        }

        #[test]
        fn any_single_content_edit_changes_it(a in ".{0,20}", b in ".{0,20}") {
            prop_assume!(a != b);
            let i = inputs("b", "h", "", &["f"]);
            let fa = fingerprint(&i, |_| Some(a.clone().into_bytes()));
            let fb = fingerprint(&i, |_| Some(b.clone().into_bytes()));
            prop_assert_ne!(fa, fb);
        }

        #[test]
        fn any_head_change_changes_it(h1 in ".{0,12}", h2 in ".{0,12}") {
            prop_assume!(h1 != h2);
            let read = |_: &str| None;
            prop_assert_ne!(
                fingerprint(&inputs("b", &h1, "", &[]), read),
                fingerprint(&inputs("b", &h2, "", &[]), read)
            );
        }
    }
}
