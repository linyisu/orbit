//! Standalone worker for applications embedding orbit-tools.
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if (args.len() != 3 && args.len() != 5) || args[0] != "--internal-document-worker" {
        return Err("document worker requires input and output paths".into());
    }
    let page = |index: usize| -> Result<Option<u32>, String> {
        match args.get(index).and_then(|value| value.to_str()) {
            None | Some("-") => Ok(None),
            Some(value) => value
                .parse()
                .map(Some)
                .map_err(|e| format!("invalid page: {e}")),
        }
    };
    orbit_tools::read_document::document_worker(
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
        page(3)?,
        page(4)?,
    )
}
