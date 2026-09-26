//! ircdocs/parser-tests conformance (vendored under tests/fixtures, CC0).

use schwaetz_proto::{CaseMapping, Message, Source, mask};
use serde::Deserialize;
use std::collections::BTreeMap;

fn fixture(name: &str) -> String {
    let path = format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[derive(Deserialize)]
struct Atoms {
    #[serde(default)]
    tags: Option<BTreeMap<String, Option<String>>>,
    #[serde(default)]
    source: Option<String>,
    verb: String,
    #[serde(default)]
    params: Vec<String>,
}

#[derive(Deserialize)]
struct SplitFile {
    tests: Vec<SplitCase>,
}
#[derive(Deserialize)]
struct SplitCase {
    input: String,
    atoms: Atoms,
}

#[test]
fn msg_split() {
    let file: SplitFile = serde_yaml::from_str(&fixture("msg-split.yaml")).unwrap();
    assert!(file.tests.len() > 30);
    for case in file.tests {
        let m = Message::parse(&case.input).unwrap_or_else(|e| panic!("{:?}: {e}", case.input));
        assert_eq!(m.command, case.atoms.verb, "verb of {:?}", case.input);
        assert_eq!(m.params, case.atoms.params, "params of {:?}", case.input);
        assert_eq!(m.source.map(|s| s.to_string()), case.atoms.source, "source of {:?}", case.input);
        let expected: BTreeMap<String, String> =
            case.atoms.tags.unwrap_or_default().into_iter().map(|(k, v)| (k, v.unwrap_or_default())).collect();
        let got: BTreeMap<String, String> = m.tags.iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        assert_eq!(got, expected, "tags of {:?}", case.input);
    }
}

#[derive(Deserialize)]
struct JoinFile {
    tests: Vec<JoinCase>,
}
#[derive(Deserialize)]
struct JoinCase {
    desc: String,
    atoms: Atoms,
    matches: Vec<String>,
}

#[test]
fn msg_join() {
    let file: JoinFile = serde_yaml::from_str(&fixture("msg-join.yaml")).unwrap();
    for case in file.tests {
        let mut m = Message::new(case.atoms.verb, case.atoms.params);
        m.source = case.atoms.source.as_deref().map(Source::parse);
        for (k, v) in case.atoms.tags.unwrap_or_default() {
            m.tags.insert(k, v.unwrap_or_default());
        }
        let line = m.to_line();
        if !case.matches.contains(&line) {
            // Tag order is unspecified; compare parsed forms instead.
            let reparsed = Message::parse(&line).unwrap();
            let ok = case.matches.iter().any(|want| {
                let w = Message::parse(want).unwrap();
                w.command == reparsed.command
                    && w.params == reparsed.params
                    && w.source == reparsed.source
                    && w.tags.len() == reparsed.tags.len()
                    && w.tags.iter().all(|(k, v)| reparsed.tags.get(k) == Some(v))
            });
            assert!(ok, "{}: produced {line:?}, expected one of {:?}", case.desc, case.matches);
        }
    }
}

#[derive(Deserialize)]
struct UserhostFile {
    tests: Vec<UserhostCase>,
}
#[derive(Deserialize)]
struct UserhostCase {
    source: String,
    atoms: UserhostAtoms,
}
#[derive(Deserialize)]
struct UserhostAtoms {
    #[serde(default)]
    nick: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    host: String,
}

#[test]
fn userhost_split() {
    let file: UserhostFile = serde_yaml::from_str(&fixture("userhost-split.yaml")).unwrap();
    for case in file.tests {
        let s = Source::parse(&case.source);
        assert_eq!(s.nick, case.atoms.nick, "{}", case.source);
        assert_eq!(s.user.unwrap_or_default(), case.atoms.user, "{}", case.source);
        assert_eq!(s.host.unwrap_or_default(), case.atoms.host, "{}", case.source);
    }
}

#[derive(Deserialize)]
struct MaskFile {
    tests: Vec<MaskCase>,
}
#[derive(Deserialize)]
struct MaskCase {
    mask: String,
    matches: Vec<String>,
    fails: Vec<String>,
}

#[test]
fn mask_match() {
    let file: MaskFile = serde_yaml::from_str(&fixture("mask-match.yaml")).unwrap();
    for case in file.tests {
        for m in &case.matches {
            assert!(mask::matches(&case.mask, m, CaseMapping::Rfc1459), "{} should match {m}", case.mask);
        }
        for m in &case.fails {
            assert!(!mask::matches(&case.mask, m, CaseMapping::Rfc1459), "{} should not match {m}", case.mask);
        }
    }
}

#[derive(Deserialize)]
struct HostFile {
    tests: Vec<HostCase>,
}
#[derive(Deserialize)]
struct HostCase {
    host: String,
    valid: bool,
}

#[test]
fn validate_hostname() {
    let file: HostFile = serde_yaml::from_str(&fixture("validate-hostname.yaml")).unwrap();
    for case in file.tests {
        assert_eq!(mask::is_valid_hostname(&case.host), case.valid, "{:?}", case.host);
    }
}

mod roundtrip {
    use proptest::prelude::*;
    use schwaetz_proto::Message;

    fn param() -> impl Strategy<Value = String> {
        "[^ :\r\n\0][^ \r\n\0]{0,12}"
    }

    proptest! {
        #[test]
        fn serialize_parse(
            cmd in "[A-Z]{1,10}|[0-9]{3}",
            middle in prop::collection::vec(param(), 0..6),
            trailing in proptest::option::of("[^\r\n\0]{0,40}"),
            tags in prop::collection::btree_map("\\+?[a-z][a-z0-9/.-]{0,10}", "[^\0\r\n]{0,10}", 0..4),
        ) {
            let mut params = middle;
            if let Some(t) = trailing { params.push(t); }
            let mut m = Message::new(cmd, params);
            for (k, v) in tags { m.tags.insert(k, v); }
            let back = Message::parse(&m.to_line()).unwrap();
            prop_assert_eq!(back, m);
        }

        #[test]
        fn parse_never_panics(s in "\\PC{0,200}") {
            let _ = Message::parse(&s);
        }
    }
}
