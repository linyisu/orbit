//! Code Assist quota discovery is a partial observation of model availability.
use super::ModelProfile;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
pub(super) fn parse(buckets: &[Value]) -> Result<Vec<ModelProfile>> {
    let mut models: Vec<ModelProfile> = Vec::new();
    // Quota buckets identify models, but omission or zero quota does not retire them.
    // https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/code_assist/types.ts
    for bucket in buckets {
        if let Some(id) = bucket["modelId"].as_str()
            && !models
                .iter()
                .any(|model| model.id == id)
        {
            ensure!(super::valid_model_id(id), "模型 ID 无效");
            models.push(serde_json::from_value(json!({
                "id": id
            }))?);
        }
    }

    Ok(models)
}
