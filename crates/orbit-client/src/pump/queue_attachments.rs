//! Resolve host attachments to client assets before allowing withdrawal/editing.
use crate::{
    protocol::{Request, Response},
    transport::Client,
};
use anyhow::{Result, bail};
use base64::Engine;
use std::collections::HashMap;
pub(super) async fn localize(
    client: &mut Client,
    session: &str,
    response: &mut Response,
    cache: &mut HashMap<(String, String), String>,
    database: &std::path::Path,
) -> Result<()> {
    let Response::InputQueue(snapshot) = response else {
        return Ok(());
    };
    let entries = match snapshot {
        orbit_protocol::input_queue::Snapshot::List { entries, .. } => entries.as_mut_slice(),
        orbit_protocol::input_queue::Snapshot::Edited { entry } => std::slice::from_mut(entry),
        _ => return Ok(()),
    };
    for entry in entries {
        for (is_image, source) in [(true, &mut entry.images), (false, &mut entry.files)] {
            let mut localized = Vec::new();
            for attachment in source.iter() {
                let key = (session.to_owned(), attachment.path.clone());
                let path = if let Some(path) = cache.get(&key) {
                    path.clone()
                } else {
                    let data = match client
                        .request(Request::ReadFile {
                            session: session.into(),
                            path: attachment.path.clone(),
                            allow_missing: true,
                        })
                        .await?
                    {
                        Response::File(data) => data,
                        Response::FileMissing => continue,
                        _ => bail!("服务未返回暂存附件"),
                    };
                    let name = attachment.name.clone();
                    let database = database.to_owned();
                    let path = tokio::task::spawn_blocking(move || -> Result<String> {
                        let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
                        let path = if is_image {
                            orbit_tools::image_assets::ImageAssets::for_database(&database)
                                .import_bytes(name, bytes)?
                                .path()
                                .to_owned()
                        } else {
                            use orbit_runtime::assets::AssetDirectory;
                            let directory = AssetDirectory::for_database(&database);
                            let extension = std::path::Path::new(&name)
                                .extension()
                                .and_then(|e| e.to_str())
                                .filter(|e| {
                                    !e.is_empty()
                                        && e.len() <= AssetDirectory::MAX_EXTENSION_BYTES
                                        && e.bytes()
                                            .all(|b| b.is_ascii_alphanumeric())
                                })
                                .unwrap_or("bin")
                                .to_ascii_lowercase();
                            directory.import(&bytes, &extension)?.0
                        };
                        Ok(path.to_string_lossy().into_owned())
                    })
                    .await??;
                    cache.insert(key, path.clone());
                    path
                };
                localized.push(orbit_protocol::MessageImage {
                    name: attachment.name.clone(),
                    path,
                });
            }
            *source = localized;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_protocol::{
        MessageImage,
        input_queue::{Entry, Snapshot, State},
    };

    fn queue() -> Response {
        Response::InputQueue(Snapshot::Edited {
            entry: Entry {
                id: 1,
                content: "synthetic queued message".into(),
                images: Vec::new(),
                files: vec![
                    MessageImage {
                        name: "missing.txt".into(),
                        path: "missing.txt".into(),
                    },
                    MessageImage {
                        name: "present.txt".into(),
                        path: "present.txt".into(),
                    },
                ],
                state: State::Idle,
            },
        })
    }

    #[tokio::test]
    async fn explicit_missing_file_preserves_the_rest_of_a_withdrawn_message() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"independent attachment body";
        let mut client = crate::transport::tests::fixture_client(vec![
            Response::FileMissing,
            Response::File(base64::engine::general_purpose::STANDARD.encode(bytes)),
        ])
        .await;
        let mut response = queue();
        localize(
            &mut client,
            "fixture-session",
            &mut response,
            &mut HashMap::new(),
            &root.path().join("cache.db"),
        )
        .await
        .unwrap();
        let Response::InputQueue(Snapshot::Edited { entry }) = response else {
            panic!("expected edited queue");
        };
        assert_eq!(entry.content, "synthetic queued message");
        assert_eq!(entry.files.len(), 1);
        assert_eq!(std::fs::read(&entry.files[0].path).unwrap(), bytes);
    }

    #[tokio::test]
    async fn legacy_missing_text_is_reported_without_guessing_or_removing_attachments() {
        let root = tempfile::tempdir().unwrap();
        let mut client = crate::transport::tests::fixture_client(vec![Response::Error(
            "not found in historical permissions".into(),
        )])
        .await;
        let mut response = queue();
        let error = localize(
            &mut client,
            "fixture-session",
            &mut response,
            &mut HashMap::new(),
            &root.path().join("cache.db"),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::transport::RequestError>(),
            Some(crate::transport::RequestError::Remote(_))
        ));
        let Response::InputQueue(Snapshot::Edited { entry }) = response else {
            panic!("expected edited queue");
        };
        assert_eq!(entry.files.len(), 2);
    }
}
