//! Bounded immutable retrieval snapshots; continuation never refetches a changing URL.

use super::{ToolError, failed, invalid};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;

const MAX_SNAPSHOT_BYTES: usize = 32 * 1_024 * 1_024;
const MAX_SNAPSHOTS: usize = 64;

#[derive(Default)]
pub(super) struct Snapshots {
    entries: Mutex<VecDeque<(String, Value, usize)>>,
    directory: Option<PathBuf>,
}

impl Snapshots {
    pub fn persistent(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
            ..Default::default()
        }
    }

    pub fn insert(&self, result: String) -> Result<String, ToolError> {
        let bytes = result.len();
        if bytes > MAX_SNAPSHOT_BYTES {
            return Err(failed("retrieved document exceeds snapshot storage budget"));
        }
        let digest: String = Sha256::digest(result.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let id = format!("web-{digest}");
        let mut snapshots = self
            .entries
            .lock()
            .expect("web snapshot lock poisoned");
        if let Some(directory) = &self.directory {
            save(directory, &id, &result)?;
        }
        let result: Value = serde_json::from_str(&result).map_err(failed)?;
        snapshots.retain(|(old, _, _)| old != &id);
        let mut used = snapshots
            .iter()
            .map(|(_, _, bytes)| bytes)
            .sum::<usize>();
        while snapshots.len() >= MAX_SNAPSHOTS || used.saturating_add(bytes) > MAX_SNAPSHOT_BYTES {
            if let Some((_, _, size)) = snapshots.pop_front() {
                used -= size;
            }
        }
        snapshots.push_back((id.clone(), result, bytes));
        Ok(id)
    }

    pub fn page(
        &self,
        id: &str,
        start: usize,
        limit: usize,
        url: &str,
    ) -> Result<String, ToolError> {
        let mut snapshots = self
            .entries
            .lock()
            .expect("web snapshot lock poisoned");
        let index = snapshots
            .iter()
            .position(|(key, _, _)| key == id);
        let mut result = if let Some(index) = index {
            let entry = snapshots.remove(index).unwrap();
            let result = entry.1.clone();
            snapshots.push_back(entry);
            result
        } else if let Some(directory) = &self.directory {
            serde_json::from_str(&read(directory, id)?).map_err(failed)?
        } else {
            return Err(failed("web snapshot expired or belongs to another session"));
        };
        drop(snapshots);
        if !url.is_empty() && result["requested_url"] != url && result["url"] != url {
            return Err(invalid("url does not match snapshot_id"));
        }
        let text = result["content"]
            .as_str()
            .ok_or_else(|| failed("invalid web snapshot"))?;
        let total = text.chars().count();
        if start > total {
            return Err(invalid(format!("start_char exceeds text length ({total})")));
        }
        let content: String = text
            .chars()
            .skip(start)
            .take(limit)
            .collect();
        let end = start + content.chars().count();
        let links = super::page_links(
            result["links"]
                .as_array()
                .map_or(&[][..], Vec::as_slice),
            text,
            start,
            end,
        );
        result["links"] = json!(links);
        result["content"] = json!(content);
        result["start_char"] = json!(start);
        result["total_chars"] = json!(total);
        result["snapshot_id"] = json!(id);
        result["truncated"] = json!(end < total);
        result["has_more"] = json!(end < total);
        result["next_start_char"] = json!((end < total).then_some(end));
        Ok(result.to_string())
    }
}

fn validate_id(id: &str) -> Result<(), ToolError> {
    if id.len() != 68
        || !id.starts_with("web-")
        || !id[4..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid("invalid web snapshot id"));
    }
    Ok(())
}

pub(super) fn read(directory: &Path, id: &str) -> Result<String, ToolError> {
    validate_id(id)?;
    let metadata = std::fs::symlink_metadata(directory).map_err(failed)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(failed("invalid snapshot directory"));
    }
    #[cfg(unix)]
    let file = orbit_runtime::filesystem::open_regular_file(&directory.join(format!("{id}.json")))
        .map_err(failed)?;
    #[cfg(windows)]
    let file = {
        orbit_runtime::filesystem::private::validate_directory(directory).map_err(failed)?;
        orbit_runtime::filesystem::private::open_file(
            &directory.join(format!("{id}.json")),
            false,
            false,
        )
        .map_err(failed)?
    };
    if !file
        .metadata()
        .map_err(failed)?
        .is_file()
    {
        return Err(failed("invalid snapshot file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SNAPSHOT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(failed("snapshot exceeds storage budget"));
    }
    String::from_utf8(bytes).map_err(failed)
}

fn save(directory: &Path, id: &str, content: &str) -> Result<(), ToolError> {
    #[cfg(unix)]
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(failed)?;
    #[cfg(windows)]
    orbit_runtime::filesystem::private::create_directory_all(directory).map_err(failed)?;
    let metadata = std::fs::symlink_metadata(directory).map_err(failed)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(failed("invalid snapshot directory"));
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("web-") || !name.ends_with(".json") {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(failed)?;
        if metadata.is_file() && name != format!("{id}.json") {
            entries.push((
                metadata.modified().map_err(failed)?,
                metadata.len(),
                entry.path(),
            ));
        }
    }
    entries.sort_by_key(|entry| entry.0);
    let mut bytes = entries
        .iter()
        .map(|entry| entry.1)
        .sum::<u64>()
        + content.len() as u64;
    let mut count = entries.len() + 1;
    for (_, size, path) in entries {
        if bytes <= MAX_SNAPSHOT_BYTES as u64 && count <= MAX_SNAPSHOTS {
            break;
        }
        std::fs::remove_file(path).map_err(failed)?;
        bytes -= size;
        count -= 1;
    }
    #[cfg(unix)]
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(failed)?;
    #[cfg(windows)]
    let mut file = orbit_runtime::filesystem::private::temporary_file(directory, ".snapshot-")
        .map_err(failed)?;
    file.write_all(content.as_bytes())
        .map_err(failed)?;
    file.as_file()
        .sync_all()
        .map_err(failed)?;
    file.persist(directory.join(format!("{id}.json")))
        .map_err(failed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_snapshots_survive_reopening_and_remain_session_scoped() {
        let directory = tempfile::tempdir().unwrap();
        let owner = directory.path().join("owner");
        let snapshots = Snapshots::persistent(owner.clone());
        let original =
            json!({"url": "https://example.org", "content": "first\nsecond\nthird", "links": []})
                .to_string();
        let id = snapshots
            .insert(original.clone())
            .unwrap();
        drop(snapshots);
        assert_eq!(read(&owner, &id).unwrap(), original);
        let reopened = Snapshots::persistent(owner.clone());
        let page: Value = serde_json::from_str(&reopened.page(&id, 6, 6, "").unwrap()).unwrap();
        assert_eq!(page["content"], "second");
        assert!(read(&directory.path().join("other"), &id).is_err());
        assert!(read(&owner, "../outside").is_err());
        std::fs::remove_file(owner.join(format!("{id}.json"))).unwrap();
        std::fs::write(directory.path().join("external"), &original).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            directory.path().join("external"),
            owner.join(format!("{id}.json")),
        )
        .unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(
            directory.path().join("external"),
            owner.join(format!("{id}.json")),
        )
        .unwrap();
        assert!(read(&owner, &id).is_err());
    }

    #[test]
    fn continuation_reads_the_original_snapshot_and_only_relevant_links() {
        let snapshots = Snapshots::default();
        let document = |body: &str| {
            json!({
                "requested_url": "https://example.org/guide",
                "url": "https://example.org/guide", "content": body,
                "links": [{ "n": 1, "url": "https://example.org/a" },
                          { "n": 2, "url": "https://example.org/b" }]
            })
            .to_string()
        };
        let original = snapshots
            .insert(document("甲[1]乙[2]"))
            .unwrap();
        snapshots
            .insert(document("changed"))
            .unwrap();
        let page: Value = serde_json::from_str(
            &snapshots
                .page(&original, 4, 4, "")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(page["content"], "乙[2]");
        assert_eq!(page["links"].as_array().unwrap().len(), 1);
        assert_eq!(page["links"][0]["n"], 2);
        assert_eq!(page["has_more"], false);
        assert!(
            snapshots
                .page(&original, 0, 4, "https://example.org/other")
                .is_err()
        );
        assert!(
            Snapshots::default()
                .page(&original, 0, 4, "")
                .is_err()
        );
    }
}
