//! Searchable model selection with independent enabled state and viewport.

#[cfg(test)]
use super::model;
use crate::config::ModelProfile;
pub(super) use orbit_config::config::connections::models::discover;

pub(super) struct Entry {
    pub profile: ModelProfile,
    pub enabled: bool,
}

pub(super) fn merge(entries: &mut Vec<Entry>, profiles: Vec<ModelProfile>) {
    for profile in profiles {
        if !entries
            .iter()
            .any(|entry| entry.profile.id == profile.id)
        {
            entries.push(Entry {
                profile: ModelProfile::new(profile.id),
                enabled: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rediscovery_keeps_selection_and_model_settings() {
        let mut profile = model::blank("a");
        profile.max_output_tokens = Some(4_096);
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
        assert_eq!(entries[0].profile.max_output_tokens, Some(4_096));
        assert_eq!(entries[0].profile.supports_vision, Some(true));
        assert!(!entries[1].enabled);
        assert!(
            entries[1]
                .profile
                .context_tokens
                .is_none()
        );
    }
}
