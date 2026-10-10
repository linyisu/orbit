#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: std::time::SystemTime,
    inode: u64,
    device: u64,
}

pub(crate) struct Preview {
    stamp: Stamp,
    pub text: std::sync::Arc<str>,
}

#[cfg(test)]
pub fn read_command_stdout(dir: &std::path::Path, output_id: &str) -> Option<String> {
    Some(read_preview(dir, output_id, None)?.text.to_string())
}

pub(crate) fn read_preview(
    dir: &std::path::Path,
    output_id: &str,
    previous: Option<Preview>,
) -> Option<Preview> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    if !output_id.starts_with("cmd-")
        || output_id.len() > 160
        || !output_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }

    let path = dir.join(format!("{output_id}.stdout"));
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let stamp = Stamp {
        length: metadata.len(),
        modified: metadata.modified().ok()?,
        inode: metadata.ino(),
        device: metadata.dev(),
    };
    if let Some(previous) = previous
        && previous.stamp == stamp
    {
        return Some(previous);
    }
    let mut file = std::fs::File::open(path).ok()?;
    let start = metadata.len().saturating_sub(16 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut content = Vec::new();
    file.take(16 * 1024).read_to_end(&mut content).ok()?;
    let skip = if start > 0 {
        content
            .iter()
            .take_while(|byte| **byte & 0xc0 == 0x80)
            .count()
    } else {
        0
    };
    Some(Preview {
        stamp,
        text: String::from_utf8_lossy(&content[skip..]).as_ref().into(),
    })
}

#[cfg(test)]
mod tests;
