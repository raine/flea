use super::{http::*, *};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ImageState {
    Processing,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DraftImage {
    pub image_id: String,
    pub position: usize,
    pub state: ImageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CategoryPrediction {
    pub category: String,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FieldOption {
    pub field: String,
    pub value: Value,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DraftModel {
    pub fields: Vec<Field>,
    pub options: Vec<FieldOption>,
    pub required_fields: Vec<String>,
    pub values: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeliveryOption {
    pub value: String,
    pub label: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_size: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DraftDelivery {
    pub source: String,
    pub available: bool,
    #[serde(default)]
    pub options: Vec<DeliveryOption>,
    #[serde(default)]
    pub option_count: usize,
    #[serde(default)]
    pub options_returned: usize,
    #[serde(default)]
    pub options_truncated: bool,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, PartialEq)]
pub struct DeliveryComposer {
    pub state: DraftDelivery,
    pub(super) source: Value,
}

impl fmt::Debug for DeliveryComposer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeliveryComposer")
            .field("state", &self.state)
            .field("source", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ComposerModelStatus {
    #[default]
    Available,
    Unavailable,
    Malformed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublicationDraftState {
    pub draft: DraftState,
    pub composer_model: ComposerModelStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCategory {
    pub category_id: String,
    pub label: String,
    pub selectable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CategoryValidation {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub exists: Option<bool>,
    pub selectable: Option<bool>,
    pub compatible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existence_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selectability_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility_source: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PublicationRequirement {
    pub field: String,
    pub reason: String,
    pub source: String,
    pub command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationEvidenceFailure {
    pub field: String,
    pub failed_stage: String,
    pub code: String,
    pub upstream_transient: bool,
    pub safe_to_retry: bool,
    pub command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublicationValidation {
    pub draft_id: String,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_validation: Option<CategoryValidation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<PublicationRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalid: Vec<PublicationRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PublicationRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverifiable: Vec<PublicationRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_failures: Vec<ValidationEvidenceFailure>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DraftState {
    pub draft_id: String,
    #[serde(default)]
    pub etag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default)]
    pub values: Map<String, Value>,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub options: Vec<FieldOption>,
    #[serde(default)]
    pub required_fields: Vec<String>,
    #[serde(default)]
    pub images: Vec<DraftImage>,
    #[serde(default)]
    pub cleared_fields: Vec<String>,
    #[serde(default)]
    pub predictions: Vec<CategoryPrediction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DraftDelivery>,
}

impl DraftState {
    pub(super) fn category_is_unset(&self) -> bool {
        self.values.get("category").is_none_or(Value::is_null)
    }

    pub(super) fn merge_model(&mut self, model: DraftModel) -> Result<(), ApiError> {
        let mut field_names = self
            .fields
            .iter()
            .map(|field| field.key.clone())
            .collect::<BTreeSet<_>>();
        for field in model.fields {
            if !field_names.insert(field.key.clone()) {
                return Err(model_error(
                    "merge_models",
                    &field.key,
                    "multiple authoritative models defined the same field",
                ));
            }
            self.fields.push(field);
        }
        self.options.extend(model.options);
        for field in model.required_fields {
            if !self.required_fields.contains(&field) {
                self.required_fields.push(field);
            }
        }
        self.values.extend(model.values);
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UploadedImage {
    pub image_id: String,
    pub state: ImageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ListingDraftSeed {
    pub listing_id: String,
    pub values: Map<String, Value>,
    #[serde(default)]
    pub images: Vec<SourceImage>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceImage {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for SourceImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceImage")
            .field("file_name", &self.file_name)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProductContext {
    pub revision: String,
    pub basic_package_urn: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Publication {
    pub listing_id: String,
    pub revision: String,
    pub state: String,
    pub order_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Confirmation {
    pub listing_id: String,
    pub order_id: String,
    #[serde(default)]
    pub details: Value,
}

pub(super) fn model_error(stage: &str, path: &str, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unrecognized_model",
        "Tori returned an unavailable or unrecognized draft model",
    );
    error.details = Some(Box::new(json!({
        "stage": stage,
        "path": path,
        "reason": reason,
    })));
    error
}
