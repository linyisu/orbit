//! Pinned account model metadata shared by selection and request projection.

use std::{collections::BTreeMap, sync::LazyLock};

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountModel {
    pub context_window: usize,
    pub input: Vec<String>,
    pub max_tokens: u32,
    pub thinking: Option<Value>,
    pub request_model_id: Option<String>,
}

pub fn antigravity_models() -> &'static BTreeMap<String, AccountModel> {
    static MODELS: LazyLock<BTreeMap<String, AccountModel>> = LazyLock::new(|| {
        #[derive(Deserialize)]
        struct Catalog {
            models: BTreeMap<String, AccountModel>,
        }

        serde_json::from_str::<Catalog>(include_str!("antigravity_models.json"))
            .expect("validated bundled Antigravity catalog")
            .models
    });

    &MODELS
}
