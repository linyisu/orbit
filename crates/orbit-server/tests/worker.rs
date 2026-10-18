use std::process::Command;

#[test]
fn headless_service_dispatches_the_document_worker() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("data.csv");
    let destination = directory.path().join("document.md");
    std::fs::write(&source, "name,value\nOrbit,42\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-workspace-server"))
        .arg("--internal-document-worker")
        .arg(&source)
        .arg(&destination)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let document = std::fs::read_to_string(&destination).unwrap();
    assert!(document.contains("Orbit"));
    assert!(document.contains("42"));
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-workspace-server"))
        .args(["--internal-document-worker"])
        .arg(&source)
        .arg(&destination)
        .args(["-", "-", r#"{"start_row":2,"max_rows":1,"sheet":null}"#])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(destination).unwrap()).unwrap();
    assert_eq!(document["table"]["returned_rows"], 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(document["text"].as_str().unwrap()).unwrap(),
        serde_json::json!([["Orbit", "42"]])
    );
}
