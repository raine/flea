use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    domain::{
        field::{Field, FieldOption, FieldType, Requirement},
        publication_form::PublicationForm,
    },
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            publication::{ListingInput, validate_input},
            publication_discovery::{DiscoveryRequest, VintedPublicationDiscoveryApi},
            search::VintedSearchSession,
        },
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublicationCategory {
    pub id: u64,
    pub title: String,
    pub path: Vec<String>,
    pub leaf: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PublicationCategoryCollection {
    pub categories: Vec<PublicationCategory>,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VintedComposer {
    pub category: PublicationCategory,
    pub form: PublicationForm,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<ComposerSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listing_input: Option<ListingInput>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ComposerSuggestion {
    pub field: String,
    pub value: Value,
    pub reason: String,
}

pub struct VintedPublicationComposer<'a> {
    session: &'a dyn VintedSearchSession,
    api: &'a dyn VintedPublicationDiscoveryApi,
}

impl<'a> VintedPublicationComposer<'a> {
    pub fn new(
        session: &'a dyn VintedSearchSession,
        api: &'a dyn VintedPublicationDiscoveryApi,
    ) -> Self {
        Self { session, api }
    }

    pub async fn compose(
        &self,
        portal: PortalId,
        category_id: u64,
        supplied: Option<Value>,
    ) -> Result<VintedComposer, AppError> {
        let credentials = self.session.credentials(portal)?;
        let catalogs = self
            .api
            .execute(&credentials, &DiscoveryRequest::Catalogs)
            .await?;
        let category = categories_from_response(&catalogs)
            .into_iter()
            .find(|category| category.id == category_id)
            .ok_or_else(|| {
                AppError::validation(
                    "vinted.category_not_found",
                    "The selected category is absent from the runtime publication catalog",
                )
            })?;
        if !category.leaf {
            return Err(AppError::validation(
                "vinted.category_not_leaf",
                "Vinted publication requires a leaf category",
            ));
        }

        let selections = json!([{"code": "category", "value": [category_id]}]);
        let attributes_request = DiscoveryRequest::Attributes { selections };
        let brands_request = DiscoveryRequest::Brands {
            category_id,
            keyword: String::new(),
        };
        let colors_request = DiscoveryRequest::Colors;
        let configuration_request = DiscoveryRequest::Configuration;
        let packages_request = DiscoveryRequest::PackageSizes { category_id };
        let (attributes, brands, colors, configuration, packages) = tokio::join!(
            self.api.execute(&credentials, &attributes_request),
            self.api.execute(&credentials, &brands_request),
            self.api.execute(&credentials, &colors_request),
            self.api.execute(&credentials, &configuration_request),
            self.api.execute(&credentials, &packages_request),
        );
        compose_from_documents(
            category,
            supplied,
            &attributes?,
            &brands?,
            &colors?,
            &configuration?,
            &packages?,
        )
    }
}

pub fn categories_from_response(response: &Value) -> Vec<PublicationCategory> {
    let mut categories = Vec::new();
    collect_categories(response, &[], &mut categories);
    categories.sort_by_key(|category| category.id);
    categories.dedup_by_key(|category| category.id);
    categories
}

pub fn categories_for_search(search: &Value, catalogs: &Value) -> Vec<PublicationCategory> {
    let ids = category_ids_from_search(search);
    let mut categories = categories_from_response(catalogs);
    if ids.is_empty() {
        return categories_from_response(search);
    }
    categories.retain(|category| ids.contains(&category.id));
    categories
}

fn category_ids_from_search(value: &Value) -> BTreeSet<u64> {
    let mut ids = BTreeSet::new();
    collect_search_ids(value, &mut ids);
    ids
}

fn collect_search_ids(value: &Value, ids: &mut BTreeSet<u64>) {
    match value {
        Value::Number(value) => {
            if let Some(value) = value.as_u64() {
                ids.insert(value);
            }
        }
        Value::String(value) => {
            if let Ok(value) = value.parse() {
                ids.insert(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_search_ids(value, ids);
            }
        }
        Value::Object(object) => {
            if let Some(id) = numeric_id(object) {
                ids.insert(id);
            }
            for (key, value) in object {
                if key.contains("catalog") || key == "id" || key == "results" || key == "data" {
                    collect_search_ids(value, ids);
                }
            }
        }
        _ => {}
    }
}

fn collect_categories(value: &Value, parents: &[String], output: &mut Vec<PublicationCategory>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_categories(value, parents, output);
            }
        }
        Value::Object(object) => {
            let id = numeric_id(object);
            let title = object_label(object);
            let children = child_values(object);
            if let (Some(id), Some(title)) = (id, title) {
                let path = explicit_path(object).unwrap_or_else(|| {
                    let mut path = parents.to_vec();
                    path.push(title.clone());
                    path
                });
                let leaf = bool_at(object, &["leaf", "is_leaf", "is_leaf_catalog"])
                    .unwrap_or(children.is_empty());
                output.push(PublicationCategory {
                    id,
                    title: title.clone(),
                    path: path.clone(),
                    leaf,
                });
                for child in children {
                    collect_categories(child, &path, output);
                }
            } else {
                for child in object.values() {
                    collect_categories(child, parents, output);
                }
            }
        }
        _ => {}
    }
}

fn compose_from_documents(
    category: PublicationCategory,
    supplied: Option<Value>,
    attributes: &Value,
    brands: &Value,
    colors: &Value,
    configuration: &Value,
    packages: &Value,
) -> Result<VintedComposer, AppError> {
    let supplied_object = match supplied.as_ref() {
        Some(Value::Object(object)) => Some(object),
        Some(_) => return Err(AppError::usage("Composer input must be a JSON object")),
        None => None,
    };
    if let Some(value) = supplied_object
        .and_then(|object| object.get("catalog_id"))
        .and_then(Value::as_u64)
        && value != category.id
    {
        return Err(AppError::validation(
            "vinted.category_mismatch",
            "Composer input catalog_id does not match the selected category",
        ));
    }

    let mut form = PublicationForm::default();
    let values = &mut form.values;
    values.insert("category".into(), json!(category.id));
    copy_scalar(supplied_object, values, "title");
    copy_scalar(supplied_object, values, "description");
    copy_scalar(supplied_object, values, "price");
    copy_scalar(supplied_object, values, "currency");
    copy_scalar_as(supplied_object, values, "package_size_id", "package_size");
    copy_scalar_as(supplied_object, values, "color_ids", "color");
    for key in [
        "isbn",
        "is_unisex",
        "ai_photo",
        "measurement_length",
        "measurement_width",
        "manufacturer",
        "manufacturer_labelling",
        "shipment_prices",
        "parcel",
    ] {
        copy_scalar(supplied_object, values, key);
    }
    if let Some(object) = supplied_object {
        let brand_id = object.get("brand_id").cloned().unwrap_or(Value::Null);
        let brand = object.get("brand").cloned().unwrap_or(Value::Null);
        if !brand_id.is_null() || !brand.is_null() {
            values.insert(
                "brand".into(),
                json!({"brand_id": brand_id, "brand": brand}),
            );
        }
        if let Some(item_attributes) = object.get("item_attributes").and_then(Value::as_array) {
            for attribute in item_attributes {
                if let Some(code) = attribute.get("code").and_then(Value::as_str) {
                    values.insert(
                        format!("attribute.{code}"),
                        attribute.get("ids").cloned().unwrap_or_else(|| json!([])),
                    );
                }
            }
        }
    }

    add_field(
        &mut form,
        "category",
        "Category",
        FieldType::Select,
        Requirement::Required,
        "classification",
    );
    form.options.push(FieldOption {
        field: "category".into(),
        value: json!(category.id),
        label: category.path.join(" > "),
        raw: Some(json!({"code": "category", "value": [category.id]})),
    });
    add_field(
        &mut form,
        "title",
        "Title",
        FieldType::String,
        Requirement::Required,
        "details",
    );
    add_field(
        &mut form,
        "description",
        "Description",
        FieldType::Text,
        Requirement::Required,
        "details",
    );
    add_field(
        &mut form,
        "price",
        "Price",
        FieldType::Decimal,
        Requirement::Required,
        "price",
    );
    add_field(
        &mut form,
        "currency",
        "Currency",
        FieldType::Select,
        Requirement::Required,
        "price",
    );
    add_field(
        &mut form,
        "brand",
        "Brand",
        FieldType::Select,
        Requirement::Required,
        "details",
    );
    add_field(
        &mut form,
        "color",
        "Color",
        FieldType::MultiSelect,
        Requirement::Required,
        "details",
    );
    add_field(
        &mut form,
        "package_size",
        "Package size",
        FieldType::Select,
        Requirement::Required,
        "shipping",
    );

    add_named_options(&mut form.options, "brand", brands, |id, label, raw| {
        FieldOption {
            field: "brand".into(),
            value: json!({"brand_id": id, "brand": label}),
            label: label.into(),
            raw: Some(raw.clone()),
        }
    });
    form.options.retain(|option| {
        !(option.field == "brand" && option.value.pointer("/brand_id") == Some(&json!(1)))
    });
    form.options.insert(
        form.options
            .iter()
            .position(|option| option.field == "brand")
            .unwrap_or(form.options.len()),
        FieldOption {
            field: "brand".into(),
            value: json!({"brand_id": 1, "brand": ""}),
            label: "No brand".into(),
            raw: Some(json!({"brand_id": 1, "brand": ""})),
        },
    );
    add_named_options(&mut form.options, "color", colors, |id, label, raw| {
        FieldOption {
            field: "color".into(),
            value: json!(id),
            label: label.into(),
            raw: Some(raw.clone()),
        }
    });
    add_named_options(
        &mut form.options,
        "package_size",
        packages,
        |id, label, raw| FieldOption {
            field: "package_size".into(),
            value: json!(id),
            label: label.into(),
            raw: Some(raw.clone()),
        },
    );

    add_dynamic_attributes(&mut form, attributes);
    add_currency_options(&mut form, configuration);
    apply_price_metadata(&mut form, configuration);
    add_optional_listing_fields(&mut form);
    summarize_options(&mut form);
    form.validate();

    let listing_input = if form.ready {
        let input = supplied
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                AppError::validation(
                    "vinted.invalid_listing_input",
                    format!("Complete composer values are not valid ListingInput JSON: {error}"),
                )
            })?;
        if let Some(input) = &input {
            validate_input(input)?;
        }
        input
    } else {
        None
    };

    let suggestions = currency_suggestion(&form);
    Ok(VintedComposer {
        category,
        form,
        suggestions,
        listing_input,
    })
}

fn add_field(
    form: &mut PublicationForm,
    key: &str,
    label: &str,
    field_type: FieldType,
    requirement: Requirement,
    section: &str,
) {
    form.fields.push(Field::new(
        key,
        label,
        field_type,
        requirement,
        form.values.get(key).cloned(),
        section,
    ));
}

fn add_optional_listing_fields(form: &mut PublicationForm) {
    for (key, label, field_type, section) in [
        ("isbn", "ISBN", FieldType::String, "details"),
        ("is_unisex", "Unisex", FieldType::Boolean, "details"),
        ("ai_photo", "AI photo", FieldType::Boolean, "photos"),
        (
            "measurement_length",
            "Measurement length",
            FieldType::Integer,
            "measurements",
        ),
        (
            "measurement_width",
            "Measurement width",
            FieldType::Integer,
            "measurements",
        ),
        ("manufacturer", "Manufacturer", FieldType::String, "details"),
        (
            "manufacturer_labelling",
            "Manufacturer labelling",
            FieldType::String,
            "details",
        ),
        (
            "shipment_prices",
            "Shipment prices",
            FieldType::Unknown("object".into()),
            "shipping",
        ),
        (
            "parcel",
            "Parcel",
            FieldType::Unknown("object".into()),
            "shipping",
        ),
    ] {
        add_field(form, key, label, field_type, Requirement::Optional, section);
    }
}

fn add_dynamic_attributes(form: &mut PublicationForm, response: &Value) {
    let mut definitions = Vec::new();
    collect_attribute_definitions(response, &mut definitions);
    for definition in definitions {
        let Some(code) = definition.get("code").and_then(Value::as_str) else {
            continue;
        };
        if code == "category" {
            continue;
        }
        let key = format!("attribute.{code}");
        let label = object_label(definition.as_object().expect("object"))
            .unwrap_or_else(|| code.to_owned());
        let required = bool_at(
            definition.as_object().expect("object"),
            &["required", "is_required"],
        )
        .unwrap_or(false);
        let requirement = if required {
            Requirement::Required
        } else {
            Requirement::Optional
        };
        add_field(
            form,
            &key,
            &label,
            FieldType::MultiSelect,
            requirement,
            "attributes",
        );
        if let Some(options) = option_array(definition.as_object().expect("object")) {
            for option in options {
                let Some(object) = option.as_object() else {
                    continue;
                };
                let (Some(id), Some(label)) = (numeric_id(object), object_label(object)) else {
                    continue;
                };
                form.options.push(FieldOption {
                    field: key.clone(),
                    value: json!(id),
                    label,
                    raw: Some(option.clone()),
                });
            }
        }
    }
}

fn collect_attribute_definitions<'a>(value: &'a Value, output: &mut Vec<&'a Value>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_attribute_definitions(value, output);
            }
        }
        Value::Object(object) => {
            if object.get("code").and_then(Value::as_str).is_some()
                && option_array(object).is_some()
            {
                output.push(value);
            } else {
                for value in object.values() {
                    collect_attribute_definitions(value, output);
                }
            }
        }
        _ => {}
    }
}

fn add_named_options<F>(options: &mut Vec<FieldOption>, field: &str, response: &Value, mut make: F)
where
    F: FnMut(u64, &str, &Value) -> FieldOption,
{
    let mut candidates = Vec::new();
    collect_named_objects(response, &mut candidates);
    for value in candidates {
        let object = value.as_object().expect("object");
        if let (Some(id), Some(label)) = (numeric_id(object), object_label(object)) {
            let mut option = make(id, &label, value);
            option.field = field.to_owned();
            if !options
                .iter()
                .any(|existing| existing.field == field && existing.value == option.value)
            {
                options.push(option);
            }
        }
    }
}

fn collect_named_objects<'a>(value: &'a Value, output: &mut Vec<&'a Value>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_named_objects(value, output);
            }
        }
        Value::Object(object) => {
            if numeric_id(object).is_some() && object_label(object).is_some() {
                output.push(value);
            } else {
                for value in object.values() {
                    collect_named_objects(value, output);
                }
            }
        }
        _ => {}
    }
}

fn add_currency_options(form: &mut PublicationForm, configuration: &Value) {
    let mut currencies = Vec::new();
    collect_strings_for_keys(
        configuration,
        &["currencies", "currency_codes", "currency"],
        &mut currencies,
    );
    currencies.sort();
    currencies.dedup();
    for currency in currencies {
        form.options.push(FieldOption {
            field: "currency".into(),
            value: json!(currency),
            label: currency,
            raw: None,
        });
    }
}

fn apply_price_metadata(form: &mut PublicationForm, configuration: &Value) {
    let minimum = find_value(configuration, &["minimum_price", "min_price", "price_min"]);
    let maximum = find_value(configuration, &["maximum_price", "max_price", "price_max"]);
    if let Some(field) = form.fields.iter_mut().find(|field| field.key == "price")
        && (minimum.is_some() || maximum.is_some())
    {
        field.raw = Some(json!({"minimum": minimum, "maximum": maximum}));
    }
}

fn currency_suggestion(form: &PublicationForm) -> Vec<ComposerSuggestion> {
    if form.values.contains_key("currency") {
        return Vec::new();
    }
    let currencies = form
        .options
        .iter()
        .filter(|option| option.field == "currency")
        .collect::<Vec<_>>();
    if currencies.len() == 1 {
        vec![ComposerSuggestion {
            field: "currency".into(),
            value: currencies[0].value.clone(),
            reason: "the runtime configuration exposes one currency".into(),
        }]
    } else {
        Vec::new()
    }
}

fn summarize_options(form: &mut PublicationForm) {
    for field in &mut form.fields {
        field.option_count = form
            .options
            .iter()
            .filter(|option| option.field == field.key)
            .count();
        field.options_returned = field.option_count;
    }
}

fn copy_scalar(source: Option<&Map<String, Value>>, target: &mut Map<String, Value>, key: &str) {
    copy_scalar_as(source, target, key, key);
}

fn copy_scalar_as(
    source: Option<&Map<String, Value>>,
    target: &mut Map<String, Value>,
    source_key: &str,
    target_key: &str,
) {
    if let Some(value) = source.and_then(|source| source.get(source_key)) {
        target.insert(target_key.into(), value.clone());
    }
}

fn numeric_id(object: &Map<String, Value>) -> Option<u64> {
    [
        "id",
        "catalog_id",
        "value_id",
        "package_size_id",
        "brand_id",
        "color_id",
    ]
    .iter()
    .find_map(|key| {
        object
            .get(*key)
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
    })
}

fn object_label(object: &Map<String, Value>) -> Option<String> {
    ["title", "name", "label", "display_name"]
        .iter()
        .find_map(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

fn child_values(object: &Map<String, Value>) -> Vec<&Value> {
    ["catalogs", "children", "subcategories", "subcatalogs"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_array))
        .map(|values| values.iter().collect())
        .unwrap_or_default()
}

fn option_array(object: &Map<String, Value>) -> Option<&Vec<Value>> {
    ["values", "options", "items"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_array))
}

fn explicit_path(object: &Map<String, Value>) -> Option<Vec<String>> {
    for key in ["path", "full_path", "breadcrumbs"] {
        match object.get(key) {
            Some(Value::String(path)) => {
                return Some(
                    path.split(['>', '/'])
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned)
                        .collect(),
                );
            }
            Some(Value::Array(parts)) => {
                let path = parts
                    .iter()
                    .filter_map(|part| {
                        part.as_str()
                            .map(str::to_owned)
                            .or_else(|| part.as_object().and_then(object_label))
                    })
                    .collect::<Vec<_>>();
                if !path.is_empty() {
                    return Some(path);
                }
            }
            _ => {}
        }
    }
    None
}

fn bool_at(object: &Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_bool))
}

fn find_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match value {
        Value::Object(object) => keys
            .iter()
            .find_map(|key| object.get(*key))
            .or_else(|| object.values().find_map(|value| find_value(value, keys))),
        Value::Array(values) => values.iter().find_map(|value| find_value(value, keys)),
        _ => None,
    }
}

fn collect_strings_for_keys(value: &Value, keys: &[&str], output: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if keys.contains(&key.as_str()) {
                    match value {
                        Value::String(value) => output.push(value.clone()),
                        Value::Array(values) => output
                            .extend(values.iter().filter_map(Value::as_str).map(str::to_owned)),
                        _ => {}
                    }
                }
                collect_strings_for_keys(value, keys, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_strings_for_keys(value, keys, output);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn documents(input: Option<Value>) -> VintedComposer {
        compose_from_documents(
            PublicationCategory { id: 4380, title: "Locks".into(), path: vec!["Cycling".into(), "Locks".into()], leaf: true },
            input,
            &json!({"attributes": [{"code":"condition","title":"Condition","required":true,"values":[{"id":6,"title":"Good"}]},{"code":"material","title":"Material","required":false,"values":[{"id":9,"title":"Steel"}]}]}),
            &json!({"brands":[{"id":22,"title":"Abus"}]}),
            &json!({"colors":[{"id":3,"title":"Black"}]}),
            &json!({"currencies":["EUR"],"minimum_price":"1.00","maximum_price":"10000.00"}),
            &json!({"package_sizes":[{"id":1,"title":"Small"}]}),
        ).unwrap()
    }

    #[test]
    fn categories_keep_runtime_identity_localized_path_and_leaf_state() {
        let result = categories_from_response(
            &json!({"catalogs":[{"id":10,"title":"Cycling","catalogs":[{"id":4380,"title":"Locks","catalogs":[]}]}]}),
        );
        assert_eq!(
            result[1],
            PublicationCategory {
                id: 4380,
                title: "Locks".into(),
                path: vec!["Cycling".into(), "Locks".into()],
                leaf: true
            }
        );
    }

    #[test]
    fn category_search_ids_resolve_through_the_localized_catalog() {
        let catalogs = json!({"catalogs":[
            {"id":10,"title":"Cycling","catalogs":[
                {"id":4380,"title":"Locks","catalogs":[]},
                {"id":4381,"title":"Lights","catalogs":[]}
            ]}
        ]});
        let result = categories_for_search(&json!({"catalog_ids":[4380]}), &catalogs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, ["Cycling", "Locks"]);
    }

    #[test]
    fn composer_keeps_legacy_fields_and_no_brand_encoding() {
        let composer = documents(None);
        for field in ["brand", "color", "attribute.condition"] {
            assert_eq!(
                composer
                    .form
                    .fields
                    .iter()
                    .find(|candidate| candidate.key == field)
                    .unwrap()
                    .requirement,
                Requirement::Required
            );
        }
        let no_brand = composer
            .form
            .options
            .iter()
            .find(|option| option.field == "brand" && option.label == "No brand")
            .unwrap();
        assert_eq!(no_brand.value, json!({"brand_id":1,"brand":""}));
        assert_eq!(
            composer
                .form
                .fields
                .iter()
                .find(|field| field.key == "attribute.material")
                .unwrap()
                .requirement,
            Requirement::Optional
        );
    }

    #[test]
    fn complete_values_emit_listing_input_without_guessing_missing_facts() {
        let input = json!({
            "title":"Lock", "description":"Steel lock", "catalog_id":4380,
            "price":"10.00", "currency":"EUR", "package_size_id":1,
            "brand_id":1, "brand":"", "color_ids":[3],
            "item_attributes":[{"code":"condition","ids":[6]}]
        });
        let composer = documents(Some(input.clone()));
        assert!(composer.form.ready);
        let emitted = serde_json::to_value(composer.listing_input.unwrap()).unwrap();
        assert_eq!(emitted["catalog_id"], input["catalog_id"]);
        assert_eq!(emitted["item_attributes"], input["item_attributes"]);
        let incomplete = documents(None);
        assert!(!incomplete.form.ready);
        assert!(incomplete.listing_input.is_none());
        assert_eq!(incomplete.suggestions[0].field, "currency");
    }
}
