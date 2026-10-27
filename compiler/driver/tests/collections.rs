use std::path::PathBuf;

#[test]
fn collection_loans_and_primitive_coherence_remain_checked() {
    let cases = [
        ("escape", "fn bad(m collections.Map<string, string>) &string { return m.get(&\"k\") }", Some("E4201")),
        ("move", "fn bad(m collections.Map<string, string>) { v := m.get(&\"k\")\n moved := m\n print(v) }", Some("E4103")),
        ("mutate", "fn bad(m collections.Map<string, string>) { var map = m\n v := map.get(&\"k\")\n map.insert(\"other\", \"v\")\n print(v) }", Some("E4101")),
        ("aliases", "fn bad(m &mut collections.Map<string, string>) { a := m.get_mut(&\"a\")\n b := m.get_mut(&\"b\")\n print(a)\n print(b) }", Some("E4101")),
        ("temporary_key", "fn good(m &collections.Map<string, string>) &string { return m.get(&\"k\") }", None),
        ("foreign_primitive", "interface Extra { fn value(&self) i32 }\nimpl Extra for i32 { fn value(&self) i32 { return 0 } }", Some("E2024")),
        ("noncopy_deref", "fn bad(value &string) string { return *value }", Some("E3079")),
        ("immutable_deref", "fn bad(value &i32) { *value = 1 }", Some("E3015")),
        ("nonreference_deref", "fn bad(value i32) { print(*value) }", Some("E3080")),
    ];
    let dir = std::env::temp_dir().join(format!("tarn-collections-loans-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body, expected) in cases {
        let file = dir.join(format!("{name}.tarn"));
        std::fs::write(&file, format!("import \"collections\"\n{body}\n")).unwrap();
        let result = tarn_driver::check(&file).unwrap();
        if let Some(code) = expected {
            assert!(result.diagnostics.iter().any(|d| d.code == code), "{name}: {:?}", result.diagnostics);
        } else {
            assert!(!result.has_errors(), "{name}: {:?}", result.diagnostics);
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ordinary_foundation_modules_have_editor_sources() {
    for module in ["collections", "console", "https", "string", "json", "websocket", "encoding", "crypto"] {
        let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../stdlib/{module}/{module}.tarn")).canonicalize().unwrap();
        let result = tarn_driver::check_editor_with_overlays(&entry, &Default::default()).unwrap();
        assert!(!result.has_errors(), "{module}: {:?}", result.diagnostics);
    }
}
