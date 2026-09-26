use schwaetz_core::buffer::{Line, LineExtra, LineFlags, LineKind};
use schwaetz_core::services::HistoryStore;
use schwaetz_store::Store;

fn line(time: i64, nick: &str, text: &str, msgid: Option<&str>) -> Line {
    Line {
        id: 0,
        time,
        kind: LineKind::Message,
        flags: LineFlags::default(),
        nick: nick.into(),
        prefix: Some('@'),
        text: text.into(),
        extra: msgid.map(|m| Box::new(LineExtra { msgid: Some(m.into()), ..Default::default() })),
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("schwaetz-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn log_load_search_and_text_logs() {
    let dir = tmp("basic");
    let mut s = Store::open(&dir.join("h.sqlite"), Some(dir.join("logs"))).unwrap();
    for i in 0..50 {
        s.log(
            "Libera",
            "#rust",
            &line(
                1_700_000_000_000 + i * 1000,
                "alice",
                &format!("message number {i} about ownership"),
                Some(&format!("id{i}")),
            ),
        );
    }
    s.log("Libera", "#rust", &line(1_700_000_100_000, "bob", "\x02borrow\x02 checker rocks", None));
    s.log("OFTC", "#debian", &line(1_700_000_100_000, "carol", "apt ownership question", None));
    // Duplicate msgid is ignored.
    s.log("Libera", "#rust", &line(1_700_000_000_000, "alice", "message number 0 about ownership", Some("id0")));
    s.flush();

    let older = s.load_before("Libera", "#RUST", 1_700_000_010_000, 5);
    assert_eq!(older.len(), 5);
    assert!(older.windows(2).all(|w| w[0].time < w[1].time), "oldest first");
    assert_eq!(&*older[4].text, "message number 9 about ownership");
    assert_eq!(older[0].msgid(), Some("id5"));
    assert_eq!(older[0].prefix, Some('@'));

    let all = s.load_before("Libera", "#rust", i64::MAX, 1000);
    assert_eq!(all.len(), 51);

    let hits = s.search("ownership", None, None, 100);
    assert_eq!(hits.len(), 51);
    let hits = s.search("ownership", Some("OFTC"), None, 100);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].1, "#debian");
    // Prefix match on the last term, formatting codes don't break matching.
    assert_eq!(s.search("borr", Some("Libera"), None, 10).len(), 1);
    // FTS syntax in user input is neutralized.
    assert!(s.search("\"unbalanced AND (", None, None, 10).is_empty());

    assert_eq!(s.last_seen("Libera"), Some(1_700_000_100_000));
    assert_eq!(s.last_seen("nope"), None);

    drop(s);
    let logs = dir.join("logs").join("Libera").join("#rust");
    let files: Vec<_> = std::fs::read_dir(&logs).unwrap().collect();
    assert!(!files.is_empty());
    let content = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
    assert!(content.contains("<@alice> message number 0 about ownership"));
    assert!(content.contains("<@bob> borrow checker rocks"), "formatting stripped");
}

#[test]
fn reopen_keeps_data() {
    let dir = tmp("reopen");
    {
        let mut s = Store::open(&dir.join("h.sqlite"), None).unwrap();
        s.log("n", "#c", &line(1000, "x", "persisted", None));
    }
    let mut s = Store::open(&dir.join("h.sqlite"), None).unwrap();
    assert_eq!(s.load_before("n", "#c", i64::MAX, 10).len(), 1);
}
