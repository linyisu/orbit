//! Historical fixture construction, independent of the production migration runner.
#[allow(dead_code)]
pub fn legacy_schema(connection: &rusqlite::Connection, version: usize) {
    const SCRIPTS: &[&str] = &[
        include_str!("../../migrations/00000000000001_sessions/up.sql"),
        include_str!("../../migrations/00000000000002_projects/up.sql"),
        include_str!("../../migrations/00000000000003_run_duration/up.sql"),
        include_str!("../../migrations/00000000000004_reasoning/up.sql"),
        include_str!("../../migrations/00000000000005_tasks_memories/up.sql"),
        include_str!("../../migrations/00000000000006_event_time/up.sql"),
        include_str!("../../migrations/00000000000007_undo/up.sql"),
        include_str!("../../migrations/00000000000008_git_baseline/up.sql"),
        include_str!("../../migrations/00000000000009_plugins/up.sql"),
        include_str!("../../migrations/00000000000010_session_memory/up.sql"),
        include_str!("../../migrations/00000000000011_archives/up.sql"),
        include_str!("../../migrations/00000000000012_checkpoints/up.sql"),
        include_str!("../../migrations/00000000000013_workspace_baseline/up.sql"),
        include_str!("../../migrations/00000000000014_message_log/up.sql"),
        include_str!("../../migrations/00000000000015_compaction/up.sql"),
        include_str!("../../migrations/00000000000016_plugin_revision/up.sql"),
        include_str!("../../migrations/00000000000017_workspace_boundary/up.sql"),
        include_str!("../../migrations/00000000000018_tabs/up.sql"),
        include_str!("../../migrations/00000000000019_runtime/up.sql"),
        include_str!("../../migrations/00000000000020_file_grants/up.sql"),
        include_str!("../../migrations/00000000000021_subagents/up.sql"),
        include_str!("../../migrations/00000000000022_side_questions/up.sql"),
        include_str!("../../migrations/00000000000023_request_receipts/up.sql"),
        include_str!("../../migrations/00000000000024_subagent_parent/up.sql"),
        include_str!("../../migrations/00000000000025_questions/up.sql"),
        include_str!("../../migrations/00000000000026_plans/up.sql"),
        include_str!("../../migrations/00000000000027_input_sequence/up.sql"),
    ];
    for script in SCRIPTS.iter().take(version) {
        connection.execute_batch(script).unwrap();
    }
}
