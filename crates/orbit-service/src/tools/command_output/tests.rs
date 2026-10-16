use super::*;

#[test]
fn command_output_preview_reads_the_requested_log_and_bounds_unicode_tail() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory
            .path()
            .join("cmd-orbit-first.stdout"),
        "Orbit first",
    )
    .unwrap();
    std::fs::write(
        directory
            .path()
            .join("cmd-orbit-second.stdout"),
        "Orbit second",
    )
    .unwrap();
    assert_eq!(
        read_command_stdout(directory.path(), "cmd-orbit-first").unwrap(),
        "Orbit first"
    );
    assert_eq!(
        read_command_stdout(directory.path(), "cmd-orbit-second").unwrap(),
        "Orbit second"
    );
    assert!(read_command_stdout(directory.path(), "../cmd-orbit-first").is_none());
    let content = format!("{}Orbit end", "轨道".repeat(6_000));
    std::fs::write(
        directory
            .path()
            .join("cmd-orbit-unicode.stdout"),
        &content,
    )
    .unwrap();
    let tail = read_command_stdout(directory.path(), "cmd-orbit-unicode").unwrap();
    assert!(tail.len() <= 16 * 1_024);
    assert!(tail.len() >= 16 * 1_024 - 3);
    assert!(tail.ends_with("Orbit end"));
    assert!(content.ends_with(&tail));
    assert!(!tail.contains('\u{fffd}'));
}
