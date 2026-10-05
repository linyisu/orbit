//! Searchable model selection with independent enabled state and viewport.
use anyhow::Result;

use super::model;
use crate::{
    config::ModelProfile,
    connections::edit::{ConnectionDraft, KeyChange},
};

pub(super) struct Entry {
    pub profile: ModelProfile,
    pub enabled: bool,
}

pub(super) fn merge(entries: &mut Vec<Entry>, profiles: Vec<ModelProfile>) {
    for profile in profiles {
        if !entries.iter().any(|entry| entry.profile.id == profile.id) {
            entries.push(Entry {
                profile,
                enabled: false,
            });
        }
    }
}

pub(super) async fn discover(
    draft: &ConnectionDraft,
    key: &KeyChange<'_>,
    directory: &std::path::Path,
) -> Result<Vec<ModelProfile>> {
    let ids = if let KeyChange::Keep = key
        && let Some(provider) = &draft.provider
    {
        if let Some(name) = &provider.credential {
            let reference = orbit_auth::CredentialRef {
                directory: directory.to_path_buf(),
                name: name.clone(),
            };

            if draft.account() {
                let payload =
                    orbit_model::discovery::discover(&reference, draft.format, &draft.endpoint)
                        .await?;
                return Ok(crate::account_models::normalize(draft.format, &payload)?.0);
            }

            orbit_model::discovery::discover_saved_api_models(
                &reference,
                &draft.endpoint,
                draft.format,
            )
            .await?
        } else {
            orbit_model::discovery::discover_api_models(
                &draft.endpoint,
                draft.format,
                &provider.api_key,
            )
            .await?
        }
    } else {
        orbit_model::discovery::discover_api_models(
            &draft.endpoint,
            draft.format,
            match key {
                KeyChange::Replace(secret) => secret,
                _ => "",
            },
        )
        .await?
    };
    Ok(ids.iter().map(|id| model::blank(id)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rediscovery_keeps_selection_and_model_settings() {
        let mut profile = model::blank("a");
        profile.max_output_tokens = Some(4096);
        profile.supports_vision = Some(true);
        let mut entries = vec![Entry {
            profile,
            enabled: true,
        }];

        merge(
            &mut entries,
            vec![model::blank("b"), model::blank("a"), model::blank("b")],
        );

        assert_eq!(entries.len(), 2);
        assert!(entries[0].enabled);
        assert_eq!(entries[0].profile.max_output_tokens, Some(4096));
        assert_eq!(entries[0].profile.supports_vision, Some(true));
        assert!(!entries[1].enabled);
        assert!(entries[1].profile.context_tokens.is_none());
    }
}
