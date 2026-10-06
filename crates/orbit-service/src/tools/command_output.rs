pub fn read_command_stdout(dir: &std::path::Path, output_id: &str) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
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
    Some(String::from_utf8_lossy(&content[skip..]).into_owned())
}

#[cfg(test)]
mod tests;
