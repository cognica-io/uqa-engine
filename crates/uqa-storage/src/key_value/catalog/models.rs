//! Model and scoring-parameter persistence.

use super::{
    decode_string, load_single_string_rows, single_str_key, string_value, KeyValueCatalog,
    StorageBackendResult, TAG_MODEL, TAG_SCORING_PARAMS,
};

impl KeyValueCatalog {
    pub(super) fn save_model_impl(&self, name: &str, json: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_MODEL, name)?, &string_value(json))
    }

    pub(super) fn load_models_impl(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_MODEL)
    }

    pub(super) fn load_model_impl(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_MODEL, name)?)?
            .map(decode_string)
            .transpose()
    }

    pub(super) fn drop_model_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_MODEL, name)?)
    }

    pub(super) fn save_scoring_params_impl(
        &self,
        name: &str,
        params_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_SCORING_PARAMS, name)?,
            &string_value(params_json),
        )
    }

    pub(super) fn load_scoring_params_impl(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_SCORING_PARAMS, name)?)?
            .map(decode_string)
            .transpose()
    }

    pub(super) fn load_all_scoring_params_impl(
        &self,
    ) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_SCORING_PARAMS)
    }

    pub(super) fn drop_scoring_params_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.store
            .delete(&single_str_key(TAG_SCORING_PARAMS, name)?)
    }
}
