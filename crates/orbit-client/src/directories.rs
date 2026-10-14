use anyhow::{Context, Result, ensure};

use super::{interaction::Progress, transport::Connection};

const MARKER: &[u8] = b"\0ORBIT-DIRECTORIES\0";

#[derive(Debug)]
pub struct Listing {
    pub path: String,
    pub directories: Vec<String>,
}

pub async fn list(connection: &Connection, path: &str, progress: &Progress) -> Result<Listing> {
    progress.report(format!("读取远端目录：{path}"));
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    let script = format!(
        r#"set -eu; p={quoted}; case "$p" in '~') p="$HOME";; '~/'*) p="$HOME/${{p#\~/}}";; esac; cd -- "$p"; printf '\000ORBIT-DIRECTORIES\000%s\000' "$PWD"; for p in ./* ./.[!.]* ./..?*; do if [ -d "$p" ]; then printf '%s\000' "${{p#./}}"; fi; done"#
    );
    let mut command = connection.target.command(
        &["sh".into(), "-c".into(), script],
        true,
        connection.interactive.as_deref(),
    )?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    super::interaction::timeout(std::time::Duration::from_secs(30), Some(progress), async {
        let result = super::command::output(command.spawn()?, Some(progress)).await?;
        ensure!(
            result.status.success(),
            "目录读取失败：{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parse(&result.stdout)
    })
    .await
}

fn parse(bytes: &[u8]) -> Result<Listing> {
    let start = bytes
        .windows(MARKER.len())
        .position(|window| window == MARKER)
        .context("目标未返回目录列表")?
        + MARKER.len();
    let mut fields = bytes[start..]
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let path = String::from_utf8(
        fields
            .next()
            .context("missing directory path")?
            .to_vec(),
    )?;
    let mut directories = fields
        .map(|field| String::from_utf8(field.to_vec()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    directories.sort();
    Ok(Listing { path, directories })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_names_are_delimited_without_shell_or_line_interpretation() {
        let listing =
            parse(b"banner\n\0ORBIT-DIRECTORIES\0/home/me\0a b\0line\nbreak\0$(touch nope)\0")
                .unwrap();
        assert_eq!(listing.path, "/home/me");
        assert_eq!(listing.directories, ["$(touch nope)", "a b", "line\nbreak"]);
    }
}
