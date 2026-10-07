use anyhow::Result;
use std::path::Path;

/// Dispatch isolated worker entry points before parsing frontend arguments.
pub fn run() -> Result<bool> {
    if std::env::args().nth(1).as_deref() == Some("--internal-document-worker") {
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        anyhow::ensure!(
            args.len() == 2 || args.len() == 4,
            "document worker requires input and output paths"
        );
        let page = |index: usize| -> Result<Option<u32>> {
            match args.get(index).and_then(|value| value.to_str()) {
                None | Some("-") => Ok(None),
                Some(value) => Ok(Some(value.parse()?)),
            }
        };
        orbit_tools::read_document::document_worker(
            Path::new(&args[0]),
            Path::new(&args[1]),
            page(2)?,
            page(3)?,
        )
        .map_err(anyhow::Error::msg)?;
        return Ok(true);
    }

    if std::env::args().nth(1).as_deref() == Some("--internal-git-push-worker") {
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        anyhow::ensure!(args.len() == 1, "push worker requires request path");
        orbit_tools::git_push::push_worker(Path::new(&args[0])).map_err(anyhow::Error::msg)?;
        return Ok(true);
    }

    Ok(false)
}
