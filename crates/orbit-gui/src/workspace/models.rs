//! Preserve connection identity while keeping the model menu two levels deep.
use std::collections::BTreeMap;

pub(super) fn groups(models: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for model in models {
        let provider = model
            .split_once("::")
            .map_or("其他", |(provider, _)| provider);
        groups
            .entry(provider.into())
            .or_default()
            .push(model.clone());
    }
    for models in groups.values_mut() {
        models.sort();
        models.dedup();
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::groups;
    #[test]
    fn model_groups_keep_same_named_models_on_distinct_connections() {
        let groups = groups(&[
            "b::same".into(),
            "a::same".into(),
            "a::other".into(),
            "local".into(),
        ]);
        assert_eq!(groups["a"], ["a::other", "a::same"]);
        assert_eq!(groups["b"], ["b::same"]);
        assert_eq!(groups["其他"], ["local"]);
    }
}
