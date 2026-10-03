//! Standalone worker for applications embedding orbit-tools.
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 || args[0] != "--internal-document-worker" {
        return Err("document worker requires input and output paths".into());
    }
    orbit_tools::read_document::document_worker(
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
    )
}
