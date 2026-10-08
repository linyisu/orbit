//! One compact writer for connection setup, editing and test fixtures.
use crate::config::ProviderSpec;
use anyhow::Result;
use toml_edit::{DocumentMut, Item};

pub fn provider(spec: &ProviderSpec) -> Result<Item> {
    let mut document = toml::to_string(spec)?.parse::<DocumentMut>()?;
    if spec.auto_refresh_models {
        document.remove("auto_refresh_models");
    }
    if spec.timeout_seconds == 600 {
        document.remove("timeout_seconds");
    }
    if spec.max_retries == orbit_model::types::default_max_retries() {
        document.remove("max_retries");
    }
    if let Some(overrides) = document
        .get_mut("model_overrides")
        .and_then(Item::as_table_mut)
    {
        for (_, item) in overrides.iter_mut() {
            if let Item::Table(table) = item {
                *item = Item::Value(std::mem::take(table).into_inline_table().into());
            }
        }
    }
    Ok(Item::Table(document.as_table().clone()))
}

pub fn models(destination: &mut Item, spec: &ProviderSpec) -> Result<()> {
    let mut fields = provider(spec)?;
    destination["models"] = fields.as_table_mut().unwrap().remove("models").unwrap();
    let table = destination
        .as_table_like_mut()
        .expect("provider is a table");
    table.remove("model_overrides");
    if let Some(overrides) = fields.as_table_mut().unwrap().remove("model_overrides") {
        table.insert("model_overrides", overrides);
    }
    Ok(())
}
