#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    length: u64,
    version: orbit_runtime::filesystem::FileVersion,
}

pub(crate) struct Preview {
    stamp: Stamp,
    pub text: std::sync::Arc<str>,
}

impl Preview {
    pub fn bytes(&self) -> u64 {
        self.stamp.length
    }
}

#[cfg(test)]
pub fn read_command_stdout(dir: &std::path::Path, output_id: &str) -> Option<String> {
    Some(
        read_preview(
            dir,
            output_id,
            orbit_protocol::tool_output::Stream::Stdout,
            None,
        )?
        .text
        .to_string(),
    )
}

pub(crate) fn read_preview(
    dir: &std::path::Path,
    output_id: &str,
    stream: orbit_protocol::tool_output::Stream,
    previous: Option<Preview>,
) -> Option<Preview> {
    use std::io::{Read, Seek, SeekFrom};
    if !output_id.starts_with("cmd-")
        || output_id.len() > 160
        || !output_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }

    let suffix = match stream {
        orbit_protocol::tool_output::Stream::Stdout => "stdout",
        orbit_protocol::tool_output::Stream::Stderr => "stderr",
    };
    let path = dir.join(format!("{output_id}.{suffix}"));
    let mut file = orbit_runtime::execution::output::open_retained_file(&path).ok()?;
    let metadata = file.metadata().ok()?;
    let stamp = Stamp {
        length: metadata.len(),
        version: orbit_runtime::filesystem::FileVersion::read(&file).ok()?,
    };
    if let Some(previous) = previous
        && previous.stamp == stamp
    {
        return Some(previous);
    }
    let start = metadata
        .len()
        .saturating_sub(16 * 1_024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut content = Vec::new();
    file.take(16 * 1_024)
        .read_to_end(&mut content)
        .ok()?;
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
        text: String::from_utf8_lossy(&content[skip..])
            .as_ref()
            .into(),
    })
}

#[cfg(test)]
mod tests;
