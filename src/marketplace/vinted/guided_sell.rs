use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::{
    domain::{
        envelope::{NextAction, Warning},
        field::FieldOption,
    },
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            categories::{self, CatalogTree, CategoryDiscovery, SearchOptions},
            composer::{PublicationCategory, VintedPublicationComposer},
            publication::ListingInput,
            publication_discovery::{DiscoveryRequest, VintedPublicationDiscoveryApi},
            search::{VintedSearchApi, VintedSearchSession},
        },
    },
};

const MAX_ATTRIBUTE_LAYERS: usize = 16;

#[derive(Clone, Debug, Deserialize)]
pub struct GuidedSellFacts {
    pub title: Option<String>,
    pub description: Option<String>,
    pub price: Option<Value>,
    pub category: Option<String>,
    pub size: Option<String>,
    pub condition: Option<String>,
    pub brand: Option<String>,
    #[serde(default, alias = "color")]
    pub colors: Vec<String>,
    pub package_size: Option<String>,
    pub currency: Option<String>,
    #[serde(default)]
    pub attributes: BTreeMap<String, SemanticValues>,
    #[serde(default, alias = "image_paths")]
    pub images: Vec<PathBuf>,
    pub isbn: Option<String>,
    pub is_unisex: Option<bool>,
    pub ai_photo: Option<bool>,
    pub measurement_length: Option<u64>,
    pub measurement_width: Option<u64>,
    pub manufacturer: Option<String>,
    pub manufacturer_labelling: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum SemanticValues {
    One(String),
    Many(Vec<String>),
}

impl SemanticValues {
    fn values(&self) -> Vec<&str> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GuidedSelections {
    values: BTreeMap<String, Vec<u64>>,
}

impl GuidedSelections {
    pub fn parse(values: &[String]) -> Result<Self, AppError> {
        let mut parsed = Self::default();
        for value in values {
            let (field, id) = value
                .split_once('=')
                .ok_or_else(|| AppError::usage("Guided selection must use FIELD=ID"))?;
            if !valid_selection_field(field) {
                return Err(AppError::usage(format!(
                    "Unsupported guided selection field `{field}`"
                )));
            }
            let id = id.parse::<u64>().map_err(|_| {
                AppError::usage(format!(
                    "Guided selection `{value}` must contain a numeric ID"
                ))
            })?;
            let selected = parsed.values.entry(field.to_owned()).or_default();
            if !selected.contains(&id) {
                selected.push(id);
            }
        }
        for field in ["category", "brand", "package_size"] {
            if parsed.values.get(field).is_some_and(|ids| ids.len() > 1) {
                return Err(AppError::usage(format!(
                    "Guided selection field `{field}` accepts one ID"
                )));
            }
        }
        Ok(parsed)
    }

    fn ids(&self, field: &str) -> Option<&[u64]> {
        self.values.get(field).map(Vec::as_slice)
    }

    fn one(&self, field: &str) -> Option<u64> {
        self.ids(field).and_then(|ids| ids.first()).copied()
    }

    fn with_choice(&self, field: &str, id: u64) -> Self {
        let mut result = self.clone();
        if field == "color" {
            let ids = result.values.entry(field.to_owned()).or_default();
            if !ids.contains(&id) {
                ids.push(id);
            }
        } else {
            result.values.insert(field.to_owned(), vec![id]);
        }
        result
    }
}

fn valid_selection_field(field: &str) -> bool {
    matches!(field, "category" | "brand" | "color" | "package_size")
        || field
            .strip_prefix("attribute.")
            .is_some_and(|code| !code.is_empty() && code.len() <= 128)
}

#[derive(Clone, Debug)]
pub struct GuidedSellRequest {
    pub input_path: PathBuf,
    pub output_path: Option<PathBuf>,
    pub images: Vec<PathBuf>,
    pub selections: GuidedSelections,
    pub marketplace_evidence: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidedSellStatus {
    Ready,
    NeedsInput,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GuidedSellOutput {
    pub status: GuidedSellStatus,
    pub mutated: bool,
    pub safe_to_retry: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<PublicationCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_discovery: Option<Value>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub resolved_values: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambiguities: Vec<GuidedAmbiguity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_mutation: Option<GuidedMutation>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GuidedMutation {
    pub operation: &'static str,
    pub listing_input: ListingInput,
    pub image_paths: Vec<PathBuf>,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GuidedAmbiguity {
    pub field: String,
    pub code: String,
    pub instruction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<GuidedChoice>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GuidedChoice {
    pub id: u64,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub command: String,
}

pub struct GuidedVintedSell<'a> {
    session: &'a dyn VintedSearchSession,
    discovery_api: &'a dyn VintedPublicationDiscoveryApi,
    search_api: &'a dyn VintedSearchApi,
}

impl<'a> GuidedVintedSell<'a> {
    pub fn new(
        session: &'a dyn VintedSearchSession,
        discovery_api: &'a dyn VintedPublicationDiscoveryApi,
        search_api: &'a dyn VintedSearchApi,
    ) -> Self {
        Self {
            session,
            discovery_api,
            search_api,
        }
    }

    pub async fn prepare(
        &self,
        portal: PortalId,
        facts: GuidedSellFacts,
        request: &GuidedSellRequest,
    ) -> Result<(GuidedSellOutput, Vec<NextAction>, Vec<Warning>), AppError> {
        let query = facts.category.as_deref().unwrap_or("");
        let options = SearchOptions {
            query,
            title: facts.title.as_deref(),
            description: facts.description.as_deref(),
            parent_id: None,
            marketplace_evidence: request.marketplace_evidence,
            limit: categories::DEFAULT_LIMIT,
            offset: 0,
        };
        if request.selections.one("category").is_none() {
            if facts.category.is_none() {
                let (output, _) = needs_fact(
                    "category",
                    "category_required",
                    "Browse current categories and add a truthful category phrase or explicitly select a current leaf.",
                );
                return Ok((output, vec![browse_roots(portal)], Vec::new()));
            }
            options.validate()?;
        }
        let credentials = self.session.credentials(portal).await?;
        let catalogs = self
            .discovery_api
            .execute(&credentials, &DiscoveryRequest::Catalogs)
            .await?;
        let tree = CatalogTree::from_response(&catalogs)?;
        let category = if let Some(category_id) = request.selections.one("category") {
            tree.get(category_id)
                .filter(|node| node.category.leaf)
                .map(|node| node.category.clone())
                .ok_or_else(|| {
                    let mut error = AppError::validation(
                        "vinted.guided_sell.category_unavailable",
                        "The selected category is absent from the current runtime leaf catalog",
                    );
                    error.next_actions.push(browse_roots(portal));
                    error
                })?
        } else {
            let discovery = categories::discover_in_tree(
                portal,
                options,
                &tree,
                self.session,
                self.discovery_api,
                self.search_api,
            )
            .await?;
            return Ok(category_ambiguity(portal, &facts, discovery, request));
        };

        let mut images = facts.images.clone();
        images.extend(request.images.iter().cloned());
        deduplicate_paths(&mut images);

        let mut partial = base_listing_value(&facts, category.id)?;
        if let Some(brand) = facts.brand.as_deref() {
            if brand.trim().is_empty() {
                partial.insert("brand_id".into(), json!(1));
                partial.insert("brand".into(), json!(""));
            } else {
                let response = self
                    .discovery_api
                    .execute(
                        &credentials,
                        &DiscoveryRequest::Brands {
                            category_id: category.id,
                            keyword: brand.to_owned(),
                        },
                    )
                    .await?;
                let matches = named_choices(&response)
                    .into_iter()
                    .filter(|(_, label)| normalize(label) == normalize(brand))
                    .collect::<Vec<_>>();
                if matches.len() == 1 {
                    partial.insert("brand_id".into(), json!(matches[0].0));
                    partial.insert("brand".into(), json!(matches[0].1));
                } else if matches.is_empty() {
                    partial.insert("brand".into(), json!(brand));
                }
            }
        }
        if let (Some(brand_id), Some(brand)) = (
            request.selections.one("brand"),
            facts
                .brand
                .as_deref()
                .filter(|brand| !brand.trim().is_empty()),
        ) {
            partial.insert("brand_id".into(), json!(brand_id));
            partial.insert("brand".into(), json!(brand));
        }
        let mut ambiguities = Vec::new();
        let composer = VintedPublicationComposer::new(self.session, self.discovery_api);
        let mut final_composer = None;

        for _ in 0..MAX_ATTRIBUTE_LAYERS {
            let composed = composer
                .compose(portal, category.id, Some(Value::Object(partial.clone())))
                .await?;
            let mut changed = false;
            let mut layer_ambiguities = Vec::new();

            for issue in &composed.form.issues {
                let field = issue.field.as_str();
                if matches!(field, "title" | "description" | "price") {
                    layer_ambiguities.push(missing_fact_ambiguity(field, &issue.code));
                    continue;
                }
                let options = composed
                    .form
                    .options
                    .iter()
                    .filter(|option| option.field == field)
                    .collect::<Vec<_>>();
                let semantics = semantic_values_for_field(&facts, &composed.form, field);
                let override_ids = request.selections.ids(field);
                match resolve_options(field, semantics.as_deref(), override_ids, &options) {
                    Resolution::Resolved(value) => {
                        apply_resolved_value(&mut partial, field, value);
                        changed = true;
                    }
                    Resolution::Ambiguous {
                        semantic,
                        choices,
                        code,
                    } => {
                        layer_ambiguities.push(option_ambiguity(
                            field,
                            code.unwrap_or(&issue.code),
                            semantic,
                            choices,
                            request,
                        ));
                    }
                    Resolution::Missing => {
                        layer_ambiguities.push(option_ambiguity(
                            field,
                            &issue.code,
                            None,
                            options,
                            request,
                        ));
                    }
                }
            }

            if changed {
                continue;
            }
            ambiguities = layer_ambiguities;
            final_composer = Some(composed);
            break;
        }

        let composed = final_composer.ok_or_else(|| {
            AppError::unexpected("Vinted guided attribute discovery exceeded its bounded depth")
        })?;
        if images.is_empty() {
            ambiguities.push(GuidedAmbiguity {
                field: "images".into(),
                code: "required".into(),
                instruction: "Add at least one image path to input.images or pass --image.".into(),
                semantic_value: None,
                choices: Vec::new(),
            });
        }

        if !ambiguities.is_empty() || !composed.form.ready {
            let next_actions = if ambiguities
                .iter()
                .any(|ambiguity| ambiguity.choices.is_empty())
            {
                vec![NextAction {
                    command: resume_command(request, &request.selections),
                }]
            } else {
                Vec::new()
            };
            return Ok((
                GuidedSellOutput {
                    status: GuidedSellStatus::NeedsInput,
                    mutated: false,
                    safe_to_retry: true,
                    category: Some(category),
                    category_discovery: None,
                    resolved_values: composed.form.values,
                    ambiguities,
                    proposed_mutation: None,
                },
                next_actions,
                Vec::new(),
            ));
        }

        let listing_input = composed
            .listing_input
            .ok_or_else(|| AppError::unexpected("A ready Vinted composer omitted ListingInput"))?;
        let command = publication_command(portal, &images);
        let mutation = GuidedMutation {
            operation: "publish",
            listing_input,
            image_paths: images,
            command: command.clone(),
        };
        Ok((
            GuidedSellOutput {
                status: GuidedSellStatus::Ready,
                mutated: false,
                safe_to_retry: true,
                category: Some(category),
                category_discovery: None,
                resolved_values: composed.form.values,
                ambiguities: Vec::new(),
                proposed_mutation: Some(mutation),
            },
            vec![NextAction { command }],
            Vec::new(),
        ))
    }
}

fn needs_fact(field: &str, code: &str, instruction: &str) -> (GuidedSellOutput, Vec<NextAction>) {
    (
        GuidedSellOutput {
            status: GuidedSellStatus::NeedsInput,
            mutated: false,
            safe_to_retry: true,
            category: None,
            category_discovery: None,
            resolved_values: Map::new(),
            ambiguities: vec![GuidedAmbiguity {
                field: field.into(),
                code: code.into(),
                instruction: instruction.into(),
                semantic_value: None,
                choices: Vec::new(),
            }],
            proposed_mutation: None,
        },
        Vec::new(),
    )
}

fn browse_roots(portal: PortalId) -> NextAction {
    NextAction {
        command: format!("flea vinted --portal {portal} category list --roots"),
    }
}

fn category_ambiguity(
    portal: PortalId,
    facts: &GuidedSellFacts,
    discovery: CategoryDiscovery,
    request: &GuidedSellRequest,
) -> (GuidedSellOutput, Vec<NextAction>, Vec<Warning>) {
    let choices = discovery
        .page
        .categories
        .iter()
        .map(|candidate| {
            let category = &candidate.node.category;
            GuidedChoice {
                id: category.id,
                label: category.path.join(" > "),
                description: None,
                command: if category.leaf {
                    resume_command(
                        request,
                        &request.selections.with_choice("category", category.id),
                    )
                } else {
                    format!(
                        "flea vinted --portal {portal} category list --parent {}",
                        category.id
                    )
                },
            }
        })
        .collect::<Vec<_>>();
    let ambiguity = GuidedAmbiguity {
        field: "category".into(),
        code: if choices.is_empty() { "no_match" } else { "selection_required" }.into(),
        instruction: "Explicitly select a current leaf that truthfully classifies the item. Branch choices browse children, not select a category. Search results are candidates, not semantic confirmation.".into(),
        semantic_value: facts.category.clone(),
        choices,
    };
    let mut actions = vec![browse_roots(portal)];
    if discovery.page.truncated {
        let mut command = format!(
            "flea vinted --portal {portal} category search --limit {} --offset {}",
            discovery.page.limit,
            discovery.page.offset + discovery.page.returned,
        );
        if request.marketplace_evidence {
            command.push_str(" --marketplace-evidence");
        }
        for (flag, value) in [
            ("--title", &facts.title),
            ("--description", &facts.description),
        ] {
            if let Some(value) = value {
                command.push_str(&format!(" {flag}={}", shell_word(value)));
            }
        }
        command.push_str(&format!(
            " -- {}",
            shell_word(facts.category.as_deref().unwrap_or(""))
        ));
        actions.push(NextAction { command });
    }
    let metadata = json!({
        "selection_required": true,
        "total": discovery.page.total,
        "returned": discovery.page.returned,
        "limit": discovery.page.limit,
        "offset": discovery.page.offset,
        "truncated": discovery.page.truncated,
        "stages": discovery.stages,
        "marketplace_evidence": discovery.marketplace_evidence.as_ref().map(|evidence| json!({
            "source": evidence.source,
            "requests": evidence.requests,
            "truncated": evidence.truncated,
            "context_fields": evidence.context_fields,
            "interpretation": "Relative listing-count support only, not classification confidence or complete category coverage",
        })),
        "suggestions": discovery.suggestions,
        "match_sources": discovery.page.categories.iter().map(|candidate| json!({
            "id": candidate.node.category.id,
            "leaf": candidate.node.category.leaf,
            "sources": candidate.match_sources,
        })).collect::<Vec<_>>(),
    });
    (
        GuidedSellOutput {
            status: GuidedSellStatus::NeedsInput,
            mutated: false,
            safe_to_retry: true,
            category: None,
            category_discovery: Some(metadata),
            resolved_values: Map::new(),
            ambiguities: vec![ambiguity],
            proposed_mutation: None,
        },
        actions,
        discovery.warnings,
    )
}

fn base_listing_value(
    facts: &GuidedSellFacts,
    category_id: u64,
) -> Result<Map<String, Value>, AppError> {
    let mut value = Map::new();
    value.insert("catalog_id".into(), json!(category_id));
    if let Some(title) = &facts.title {
        value.insert("title".into(), json!(title));
    }
    if let Some(description) = &facts.description {
        value.insert("description".into(), json!(description));
    }
    if let Some(price) = &facts.price {
        let price = match price {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            _ => {
                return Err(AppError::validation(
                    "vinted.guided_sell.invalid_price",
                    "Semantic price must be a JSON string or number",
                ));
            }
        };
        value.insert("price".into(), json!(price));
    }
    if let Some(currency) = &facts.currency {
        value.insert("currency".into(), json!(currency));
    }
    for (key, candidate) in [
        ("isbn", facts.isbn.as_ref().map(|value| json!(value))),
        ("is_unisex", facts.is_unisex.map(|value| json!(value))),
        ("ai_photo", facts.ai_photo.map(|value| json!(value))),
        (
            "measurement_length",
            facts.measurement_length.map(|value| json!(value)),
        ),
        (
            "measurement_width",
            facts.measurement_width.map(|value| json!(value)),
        ),
        (
            "manufacturer",
            facts.manufacturer.as_ref().map(|value| json!(value)),
        ),
        (
            "manufacturer_labelling",
            facts
                .manufacturer_labelling
                .as_ref()
                .map(|value| json!(value)),
        ),
    ] {
        if let Some(candidate) = candidate {
            value.insert(key.into(), candidate);
        }
    }
    Ok(value)
}

fn semantic_values_for_field(
    facts: &GuidedSellFacts,
    form: &crate::domain::publication_form::PublicationForm,
    field: &str,
) -> Option<Vec<String>> {
    match field {
        "brand" => facts.brand.as_ref().map(|value| vec![value.clone()]),
        "color" => (!facts.colors.is_empty()).then(|| facts.colors.clone()),
        "package_size" => facts.package_size.as_ref().map(|value| vec![value.clone()]),
        "currency" => facts.currency.as_ref().map(|value| vec![value.clone()]),
        _ => {
            let code = field.strip_prefix("attribute.")?;
            if normalize(code) == "size"
                && let Some(value) = &facts.size
            {
                return Some(vec![value.clone()]);
            }
            if normalize(code) == "condition"
                && let Some(value) = &facts.condition
            {
                return Some(vec![value.clone()]);
            }
            let label = form
                .fields
                .iter()
                .find(|candidate| candidate.key == field)
                .map(|candidate| normalize(&candidate.label));
            facts.attributes.iter().find_map(|(key, values)| {
                (normalize(key) == normalize(code)
                    || label.as_ref().is_some_and(|label| normalize(key) == *label))
                .then(|| values.values().into_iter().map(str::to_owned).collect())
            })
        }
    }
}

enum Resolution<'a> {
    Resolved(Value),
    Ambiguous {
        semantic: Option<String>,
        code: Option<&'static str>,
        choices: Vec<&'a FieldOption>,
    },
    Missing,
}

fn resolve_options<'a>(
    field: &str,
    semantics: Option<&[String]>,
    override_ids: Option<&[u64]>,
    options: &[&'a FieldOption],
) -> Resolution<'a> {
    if let Some(ids) = override_ids {
        let selected = ids
            .iter()
            .filter_map(|id| options.iter().find(|option| option_id(option) == Some(*id)))
            .copied()
            .collect::<Vec<_>>();
        if selected.len() != ids.len() {
            return Resolution::Ambiguous {
                semantic: None,
                code: None,
                choices: options.to_vec(),
            };
        }
        return Resolution::Resolved(resolved_option_value(field, &selected));
    }

    let Some(semantics) = semantics else {
        if field == "currency" && options.len() == 1 {
            return Resolution::Resolved(options[0].value.clone());
        }
        return Resolution::Missing;
    };
    if field == "brand"
        && semantics.len() == 1
        && semantics[0].trim().is_empty()
        && let Some(option) = options.iter().find(|option| option_id(option) == Some(1))
    {
        return Resolution::Resolved(option.value.clone());
    }

    let mut selected = Vec::new();
    for semantic in semantics {
        let matches = options
            .iter()
            .filter(|option| {
                normalize(&option.label) == normalize(semantic)
                    || option.raw.as_ref().is_some_and(|raw| {
                        super::semantic_values::matches_runtime_alias(raw, semantic)
                    })
            })
            .copied()
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Resolution::Ambiguous {
                semantic: Some(semantic.clone()),
                code: Some(if matches.is_empty() {
                    "unmatched_semantic_value"
                } else {
                    "ambiguous_semantic_value"
                }),
                choices: if matches.is_empty() {
                    options.to_vec()
                } else {
                    matches
                },
            };
        }
        selected.push(matches[0]);
    }
    Resolution::Resolved(resolved_option_value(field, &selected))
}

fn resolved_option_value(field: &str, options: &[&FieldOption]) -> Value {
    if matches!(field, "brand" | "currency" | "package_size") {
        options[0].value.clone()
    } else {
        Value::Array(options.iter().map(|option| option.value.clone()).collect())
    }
}

fn apply_resolved_value(partial: &mut Map<String, Value>, field: &str, value: Value) {
    match field {
        "brand" => {
            partial.insert(
                "brand_id".into(),
                value.get("brand_id").cloned().unwrap_or(Value::Null),
            );
            partial.insert(
                "brand".into(),
                value.get("brand").cloned().unwrap_or(Value::Null),
            );
        }
        "color" => {
            partial.insert("color_ids".into(), value);
        }
        "package_size" => {
            partial.insert("package_size_id".into(), value);
        }
        "currency" => {
            partial.insert("currency".into(), value);
        }
        _ => {
            let Some(code) = field.strip_prefix("attribute.") else {
                return;
            };
            let attributes = partial
                .entry("item_attributes")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("guided attributes use an array");
            attributes.retain(|attribute| attribute.get("code") != Some(&json!(code)));
            attributes.push(json!({"code": code, "ids": value}));
        }
    }
}

fn missing_fact_ambiguity(field: &str, code: &str) -> GuidedAmbiguity {
    GuidedAmbiguity {
        field: field.into(),
        code: code.into(),
        instruction: format!("Add the truthful seller-provided `{field}` fact to the input."),
        semantic_value: None,
        choices: Vec::new(),
    }
}

fn option_ambiguity(
    field: &str,
    code: &str,
    semantic: Option<String>,
    options: Vec<&FieldOption>,
    request: &GuidedSellRequest,
) -> GuidedAmbiguity {
    let choices = options
        .into_iter()
        .filter_map(|option| {
            let id = option_id(option)?;
            Some(GuidedChoice {
                id,
                label: option.label.clone(),
                description: if field == "package_size" {
                    package_description(option)
                } else {
                    None
                },
                command: resume_command(request, &request.selections.with_choice(field, id)),
            })
        })
        .collect();
    GuidedAmbiguity {
        field: field.into(),
        code: code.into(),
        instruction: if semantic.is_some() {
            "Choose a scoped runtime option. Flea resolves exact labels and runtime-provided aliases, not translations."
                .into()
        } else {
            "Choose a scoped runtime option that matches the seller's facts.".into()
        },
        semantic_value: semantic,
        choices,
    }
}

fn option_id(option: &FieldOption) -> Option<u64> {
    option
        .value
        .as_u64()
        .or_else(|| option.value.get("brand_id").and_then(Value::as_u64))
}

fn named_choices(value: &Value) -> Vec<(u64, String)> {
    fn collect(value: &Value, output: &mut Vec<(u64, String)>) {
        match value {
            Value::Array(values) => {
                for value in values {
                    collect(value, output);
                }
            }
            Value::Object(object) => {
                let id = object
                    .get("id")
                    .or_else(|| object.get("brand_id"))
                    .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()));
                let label = ["title", "name", "label", "display_name"]
                    .iter()
                    .find_map(|key| object.get(*key).and_then(Value::as_str));
                if let (Some(id), Some(label)) = (id, label) {
                    output.push((id, label.to_owned()));
                } else {
                    for value in object.values() {
                        collect(value, output);
                    }
                }
            }
            _ => {}
        }
    }
    let mut output = Vec::new();
    collect(value, &mut output);
    output
}

fn normalize(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn deduplicate_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = std::collections::BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

fn package_description(option: &FieldOption) -> Option<String> {
    let description = option.raw.as_ref()?.get("description")?.as_str()?.trim();
    if description.is_empty() {
        return None;
    }
    let mut result = description.chars().take(512).collect::<String>();
    if description.chars().count() > 512 {
        result.push_str("...");
    }
    Some(result)
}

fn resume_command(request: &GuidedSellRequest, selections: &GuidedSelections) -> String {
    let mut command = format!(
        "flea vinted sell --input {}",
        shell_word(&if request.input_path.as_os_str() == "-" {
            "<saved-facts.json>".into()
        } else {
            request.input_path.to_string_lossy()
        })
    );
    if let Some(path) = &request.output_path {
        command.push_str(" --output=");
        command.push_str(&shell_word(&path.to_string_lossy()));
    }
    if request.marketplace_evidence {
        command.push_str(" --marketplace-evidence");
    }
    for image in &request.images {
        command.push_str(" --image ");
        command.push_str(&shell_word(&image.to_string_lossy()));
    }
    for (field, ids) in &selections.values {
        for id in ids {
            command.push_str(" --select ");
            command.push_str(&shell_word(&format!("{field}={id}")));
        }
    }
    command
}

fn publication_command(portal: PortalId, images: &[PathBuf]) -> String {
    let mut command = format!("flea vinted --portal {portal} publish --input listing.json");
    for image in images {
        command.push_str(" --image ");
        command.push_str(&shell_word(&image.to_string_lossy()));
    }
    command
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condition_matching_distinguishes_unmatched_multiple_aliases_and_explicit_ids() {
        let options = [
            FieldOption {
                field: "attribute.condition".into(),
                value: json!(2),
                label: "Erittäin hyvä".into(),
                raw: Some(json!({"code":"very_good"})),
            },
            FieldOption {
                field: "attribute.condition".into(),
                value: json!(3),
                label: "Hyvä".into(),
                raw: Some(json!({"code":"good"})),
            },
        ];
        let refs = options.iter().collect::<Vec<_>>();
        assert!(
            matches!(resolve_options("attribute.condition", Some(&["used".into()]), None, &refs), Resolution::Ambiguous { code: Some("unmatched_semantic_value"), choices, .. } if choices.len() == 2)
        );
        assert!(
            matches!(resolve_options("attribute.condition", Some(&["very good".into()]), None, &refs), Resolution::Resolved(value) if value == json!([2]))
        );
        assert!(
            matches!(resolve_options("attribute.condition", Some(&["used".into()]), Some(&[3]), &refs), Resolution::Resolved(value) if value == json!([3]))
        );
        assert!(matches!(
            resolve_options("attribute.condition", None, Some(&[99]), &refs),
            Resolution::Ambiguous {
                code: None,
                semantic: None,
                ..
            }
        ));
        let mut localized_only = options[0].clone();
        localized_only.raw = None;
        assert!(matches!(
            resolve_options(
                "attribute.condition",
                Some(&["very good".into()]),
                None,
                &[&localized_only]
            ),
            Resolution::Ambiguous {
                code: Some("unmatched_semantic_value"),
                ..
            }
        ));
        let mut duplicate = options[0].clone();
        duplicate.value = json!(4);
        let refs = vec![&options[0], &duplicate];
        assert!(
            matches!(resolve_options("attribute.condition", Some(&["very_good".into()]), None, &refs), Resolution::Ambiguous { code: Some("ambiguous_semantic_value"), choices, .. } if choices.len() == 2)
        );
    }

    #[test]
    fn package_choices_include_only_bounded_upstream_descriptions() {
        let request = GuidedSellRequest {
            input_path: "facts.json".into(),
            output_path: Some("saved proposal.json".into()),
            images: vec![],
            selections: GuidedSelections::default(),
            marketplace_evidence: false,
        };
        let mut option = FieldOption {
            field: "package_size".into(),
            value: json!(1),
            label: "Small".into(),
            raw: Some(json!({"description":"Runtime package guidance", "unknown":"not copied"})),
        };
        let ambiguity = option_ambiguity("package_size", "required", None, vec![&option], &request);
        assert_eq!(
            ambiguity.choices[0].description.as_deref(),
            Some("Runtime package guidance")
        );
        assert!(
            ambiguity.choices[0]
                .command
                .contains("--output='saved proposal.json'")
        );
        assert!(
            ambiguity.choices[0]
                .command
                .contains("--select 'package_size=1'")
        );
        option.raw = Some(json!({"description":"ä".repeat(600)}));
        assert_eq!(package_description(&option).unwrap().chars().count(), 515);
        for raw in [
            None,
            Some(json!({})),
            Some(json!({"description":null})),
            Some(json!({"description":" "})),
        ] {
            option.raw = raw;
            assert!(package_description(&option).is_none());
        }
    }

    #[test]
    fn exact_matching_is_case_and_unicode_normalized_without_fuzzy_guesses() {
        let options = [FieldOption {
            field: "attribute.size".into(),
            value: json!(42),
            label: "Koko 42".into(),
            raw: None,
        }];
        let references = options.iter().collect::<Vec<_>>();
        assert!(matches!(
            resolve_options(
                "attribute.size",
                Some(&["  KOKO 42 ".into()]),
                None,
                &references
            ),
            Resolution::Resolved(value) if value == json!([42])
        ));
        assert!(matches!(
            resolve_options("attribute.size", Some(&["42".into()]), None, &references),
            Resolution::Ambiguous { .. }
        ));
    }

    #[test]
    fn category_choices_are_bounded_and_branches_browse_with_contextual_pagination() {
        use crate::marketplace::vinted::categories::{DiscoveryStages, StageStatus};
        let roots = (1..32)
            .map(|id| json!({"id":id,"title":format!("Root {id:02}"),"catalogs":if id == 1 {vec![json!({"id":100,"title":"Child","catalogs":[]})]} else {vec![]}}))
            .collect::<Vec<_>>();
        let tree = CatalogTree::from_response(&json!({"catalogs":roots})).unwrap();
        let page = tree.browse(None, 20, 0).unwrap();
        let discovery = CategoryDiscovery {
            page,
            suggestions: Vec::new(),
            marketplace_evidence: None,
            stages: DiscoveryStages {
                local_catalog: StageStatus::Complete,
                publication_search: StageStatus::Empty,
                marketplace: StageStatus::NotRequested,
            },
            warnings: Vec::new(),
            selection_required: true,
        };
        let facts: GuidedSellFacts = serde_json::from_value(
            json!({"category":"Root","title":"Seller's title","description":"Original text"}),
        )
        .unwrap();
        let request = GuidedSellRequest {
            marketplace_evidence: false,
            input_path: "facts.json".into(),
            output_path: None,
            images: vec![],
            selections: GuidedSelections::default(),
        };
        let (output, actions, warnings) =
            category_ambiguity(PortalId::Fi, &facts, discovery, &request);
        assert!(warnings.is_empty());
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|action| {
            output.ambiguities[0]
                .choices
                .iter()
                .all(|choice| choice.command != action.command)
        }));
        assert_eq!(output.ambiguities[0].choices.len(), 20);
        assert_eq!(output.ambiguities[0].choices[0].label, "Root 01");
        assert_eq!(
            output.ambiguities[0].choices[0].command,
            "flea vinted --portal fi category list --parent 1"
        );
        assert_eq!(output.category_discovery.as_ref().unwrap()["total"], 31);
        let continuation = &actions.last().unwrap().command;
        assert!(continuation.contains("--limit 20 --offset 20"));
        assert!(continuation.contains(&format!("--title={}", shell_word("Seller's title"))));
        assert!(continuation.contains("--description='Original text'"));
        assert!(output.proposed_mutation.is_none());
    }

    #[test]
    fn consumed_stdin_is_never_replayed_in_resume_commands() {
        let request = GuidedSellRequest {
            marketplace_evidence: false,
            input_path: "-".into(),
            output_path: None,
            images: vec!["front.jpg".into(), "back.jpg".into()],
            selections: GuidedSelections::parse(&["brand=12".into()]).unwrap(),
        };
        let command = resume_command(&request, &request.selections.with_choice("category", 42));
        assert!(command.contains("--input '<saved-facts.json>'"));
        assert!(!command.contains("--input '-'"));
        assert!(command.contains("--image 'front.jpg' --image 'back.jpg'"));
        assert!(command.contains("--select 'brand=12' --select 'category=42'"));
    }

    #[test]
    fn resume_commands_preserve_scoped_selections_and_images() {
        let request = GuidedSellRequest {
            marketplace_evidence: false,
            input_path: "facts file.json".into(),
            output_path: None,
            images: vec!["front photo.jpg".into()],
            selections: GuidedSelections::parse(&["category=12".into()]).unwrap(),
        };
        let selected = request.selections.with_choice("attribute.size", 42);
        assert_eq!(
            resume_command(&request, &selected),
            "flea vinted sell --input 'facts file.json' --image 'front photo.jpg' --select 'attribute.size=42' --select 'category=12'"
        );
    }
}
