use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Map, Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::{domain::field::ValidationIssue, error::AppError};

const SEMANTIC_KEYS: [&str; 4] = ["size", "condition", "colors", "package_size"];
const ALIAS_KEYS: [&str; 8] = [
    "title",
    "name",
    "label",
    "display_name",
    "code",
    "value",
    "machine_value",
    "slug",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SemanticValueResolution {
    pub field: String,
    pub supplied: String,
    pub label: String,
    pub scope: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticListingResolution {
    pub input: Value,
    pub issues: Vec<ValidationIssue>,
    pub resolved: Vec<SemanticValueResolution>,
}

#[derive(Clone, Debug)]
struct RuntimeOption {
    id: u64,
    label: String,
    aliases: BTreeSet<String>,
}

pub fn has_semantic_values(input: &Value) -> bool {
    input
        .as_object()
        .is_some_and(|object| SEMANTIC_KEYS.iter().any(|key| object.contains_key(*key)))
}

pub fn resolve_semantic_listing_values(
    mut input: Value,
    attributes: &Value,
    colors: &Value,
    packages: &Value,
) -> Result<SemanticListingResolution, AppError> {
    let object = input
        .as_object_mut()
        .ok_or_else(|| AppError::usage("Composer input must be a JSON object"))?;
    let mut issues = Vec::new();
    let mut resolved = Vec::new();

    for code in ["size", "condition"] {
        let Some(value) = object.remove(code) else {
            continue;
        };
        let field = format!("attribute.{code}");
        let supplied = semantic_string(&field, value, &mut issues);
        let options = attribute_options(attributes, code);
        if let Some(supplied) = supplied
            && let Some(option) = resolve_one(&field, &supplied, "selection", &options, &mut issues)
        {
            set_attribute(object, code, option.id, &supplied, &field, &mut issues);
            resolved.push(resolution(&field, supplied, option, "selection"));
        }
    }

    if let Some(value) = object.remove("colors") {
        let field = "color";
        let supplied = semantic_strings(field, value, &mut issues);
        let options = named_options(colors);
        let mut ids = Vec::new();
        for supplied in supplied {
            if let Some(option) = resolve_one(field, &supplied, "portal", &options, &mut issues) {
                if !ids.contains(&option.id) {
                    ids.push(option.id);
                }
                resolved.push(resolution(field, supplied, option, "portal"));
            }
        }
        if !ids.is_empty() {
            set_ids(object, "color_ids", ids, field, &mut issues);
        }
    }

    if let Some(value) = object.remove("package_size") {
        let field = "package_size";
        let supplied = semantic_string(field, value, &mut issues);
        let options = named_options(packages);
        if let Some(supplied) = supplied
            && let Some(option) = resolve_one(field, &supplied, "category", &options, &mut issues)
        {
            set_id(object, "package_size_id", option.id, field, &mut issues);
            resolved.push(resolution(field, supplied, option, "category"));
        }
    }

    Ok(SemanticListingResolution {
        input,
        issues,
        resolved,
    })
}

fn semantic_string(field: &str, value: Value, issues: &mut Vec<ValidationIssue>) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value),
        _ => {
            issues.push(issue(
                field,
                "semantic_invalid_type",
                "Semantic listing values must be nonempty strings",
                json!({"supplied": value}),
            ));
            None
        }
    }
}

fn semantic_strings(field: &str, value: Value, issues: &mut Vec<ValidationIssue>) -> Vec<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => vec![value],
        Value::Array(values) => {
            let empty = values.is_empty();
            let mut output = Vec::new();
            for value in values {
                if let Some(value) = semantic_string(field, value, issues) {
                    output.push(value);
                }
            }
            if empty {
                issues.push(issue(
                    field,
                    "semantic_invalid_type",
                    "Semantic colors must contain at least one nonempty string",
                    json!({"supplied": []}),
                ));
            }
            output
        }
        value => {
            semantic_string(field, value, issues);
            Vec::new()
        }
    }
}

fn resolve_one<'a>(
    field: &str,
    supplied: &str,
    scope: &'static str,
    options: &'a [RuntimeOption],
    issues: &mut Vec<ValidationIssue>,
) -> Option<&'a RuntimeOption> {
    let normalized = normalize(supplied);
    let mut matches = options
        .iter()
        .filter(|option| option.aliases.contains(&normalized))
        .collect::<Vec<_>>();
    matches.sort_by_key(|option| option.id);
    matches.dedup_by_key(|option| option.id);
    match matches.as_slice() {
        [option] => Some(*option),
        [] => {
            issues.push(issue(
                field,
                "semantic_unavailable",
                "The semantic value is unavailable in the live scoped option catalog",
                json!({
                    "scope": scope,
                    "supplied": supplied,
                    "available_labels": options.iter().map(|option| &option.label).collect::<Vec<_>>()
                }),
            ));
            None
        }
        matches => {
            issues.push(issue(
                field,
                "semantic_ambiguous",
                "The semantic value matches more than one live scoped option",
                json!({
                    "scope": scope,
                    "supplied": supplied,
                    "matching_labels": matches.iter().map(|option| &option.label).collect::<Vec<_>>()
                }),
            ));
            None
        }
    }
}

fn resolution(
    field: &str,
    supplied: String,
    option: &RuntimeOption,
    scope: &'static str,
) -> SemanticValueResolution {
    SemanticValueResolution {
        field: field.to_owned(),
        supplied,
        label: option.label.clone(),
        scope,
    }
}

fn set_attribute(
    object: &mut Map<String, Value>,
    code: &str,
    id: u64,
    supplied: &str,
    field: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let attributes = object
        .entry("item_attributes")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(attributes) = attributes.as_array_mut() else {
        issues.push(issue(
            field,
            "semantic_conflict",
            "item_attributes must be an array when semantic attributes are supplied",
            json!({"supplied": supplied}),
        ));
        return;
    };
    if let Some(existing) = attributes
        .iter()
        .find(|attribute| attribute.get("code") == Some(&json!(code)))
    {
        let matches = existing
            .get("ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.as_slice() == [json!(id)]);
        if !matches {
            issues.push(issue(
                field,
                "semantic_conflict",
                "The semantic value conflicts with the explicit opaque attribute ID",
                json!({"supplied": supplied}),
            ));
        }
        return;
    }
    attributes.push(json!({"code": code, "ids": [id]}));
}

fn set_id(
    object: &mut Map<String, Value>,
    key: &str,
    id: u64,
    field: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    if let Some(existing) = object.get(key) {
        if existing.as_u64() != Some(id) {
            issues.push(issue(
                field,
                "semantic_conflict",
                "The semantic value conflicts with the explicit opaque ID",
                json!({"explicit_id": existing}),
            ));
        }
    } else {
        object.insert(key.to_owned(), json!(id));
    }
}

fn set_ids(
    object: &mut Map<String, Value>,
    key: &str,
    ids: Vec<u64>,
    field: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    if let Some(existing) = object.get(key) {
        let expected = ids.iter().map(|id| json!(id)).collect::<Vec<_>>();
        if existing.as_array() != Some(&expected) {
            issues.push(issue(
                field,
                "semantic_conflict",
                "The semantic values conflict with the explicit opaque IDs",
                json!({"explicit_ids": existing}),
            ));
        }
    } else {
        object.insert(key.to_owned(), json!(ids));
    }
}

fn issue(field: &str, code: &str, message: &str, raw: Value) -> ValidationIssue {
    ValidationIssue {
        field: field.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        source: Some("runtime_discovery".into()),
        raw: Some(raw),
    }
}

fn attribute_options(response: &Value, wanted_code: &str) -> Vec<RuntimeOption> {
    let options = super::composer::publication_attribute_definitions(response)
        .into_iter()
        .filter(|(code, _)| *code == wanted_code)
        .flat_map(|(_, definition)| super::composer::publication_attribute_options(definition))
        .filter_map(Value::as_object)
        .filter_map(runtime_option)
        .collect();
    deduplicate(options)
}

fn named_options(response: &Value) -> Vec<RuntimeOption> {
    let mut output = Vec::new();
    collect_named_options(response, &mut output);
    deduplicate(output)
}

fn collect_named_options(value: &Value, output: &mut Vec<RuntimeOption>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_named_options(value, output);
            }
        }
        Value::Object(object) => {
            if let Some(option) = runtime_option(object) {
                output.push(option);
            } else {
                for value in object.values() {
                    collect_named_options(value, output);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn matches_runtime_alias(raw: &Value, supplied: &str) -> bool {
    let normalized = normalize(supplied);
    !normalized.is_empty()
        && ALIAS_KEYS.iter().any(|key| {
            raw.get(*key)
                .and_then(Value::as_str)
                .is_some_and(|alias| normalize(alias) == normalized)
        })
}

fn runtime_option(object: &Map<String, Value>) -> Option<RuntimeOption> {
    let id = ["id", "value_id", "package_size_id", "color_id"]
        .iter()
        .find_map(|key| {
            object
                .get(*key)
                .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        })?;
    let label = ["title", "name", "label", "display_name"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))?
        .to_owned();
    let aliases = ALIAS_KEYS
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_str))
        .map(normalize)
        .filter(|value| !value.is_empty())
        .collect();
    Some(RuntimeOption { id, label, aliases })
}

fn deduplicate(options: Vec<RuntimeOption>) -> Vec<RuntimeOption> {
    let mut output: Vec<RuntimeOption> = Vec::new();
    for option in options {
        if let Some(existing) = output.iter_mut().find(|existing| existing.id == option.id) {
            existing.aliases.extend(option.aliases);
        } else {
            output.push(option);
        }
    }
    output
}

fn normalize(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.nfkc().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if separator && !output.is_empty() {
                output.push(' ');
            }
            output.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Value {
        json!({
            "size":"43",
            "condition":"Tyydyttävä",
            "colors":["Musta", "grey"],
            "package_size":"medium"
        })
    }

    fn attributes(size_id: u64) -> Value {
        json!({"attributes":[
            {"code":"size","configuration":{"options":[
                {"id":size_id,"title":"43","value":"eu_43"}
            ]}},
            {"code":"condition","configuration":{"options":[
                {"id":6,"title":"Tyydyttävä","code":"satisfactory"}
            ]}}
        ]})
    }

    #[test]
    fn semantic_attribute_choices_exclude_group_headings_with_aliased_ids() {
        let raw = json!({"attributes":[{"code":"condition","configuration":{"options":[
            {"id":1,"title":"Condition","type":"group","options":[
                {"id":1,"title":"Uusi ilman hintalappua","type":"default"}
            ]}
        ]}}]});
        let options = attribute_options(&raw, "condition");
        assert_eq!(options.len(), 1);
        let result = resolve_semantic_listing_values(
            json!({"condition":"Condition"}),
            &raw,
            &json!({}),
            &json!({}),
        )
        .unwrap();
        assert!(!result.issues.is_empty());
        assert!(result.input.get("item_attributes").is_none());
        let result = resolve_semantic_listing_values(
            json!({"condition":"Uusi ilman hintalappua"}),
            &raw,
            &json!({}),
            &json!({}),
        )
        .unwrap();
        assert!(result.issues.is_empty());
        assert_eq!(
            result.input["item_attributes"],
            json!([{"code":"condition","ids":[1]}])
        );
    }

    #[test]
    fn resolves_localized_labels_and_canonical_machine_values_in_live_scopes() {
        let result = resolve_semantic_listing_values(
            input(),
            &attributes(430),
            &json!({"colors":[
                {"id":3,"title":"Musta","code":"black"},
                {"id":4,"title":"Harmaa","code":"grey"}
            ]}),
            &json!({"package_sizes":[
                {"id":2,"title":"Keskikokoinen","code":"medium"}
            ]}),
        )
        .unwrap();

        assert!(result.issues.is_empty());
        assert_eq!(result.input["color_ids"], json!([3, 4]));
        assert_eq!(result.input["package_size_id"], 2);
        assert_eq!(
            result.input["item_attributes"],
            json!([
                {"code":"size","ids":[430]},
                {"code":"condition","ids":[6]}
            ])
        );
        assert_eq!(
            result
                .resolved
                .iter()
                .map(|value| value.scope)
                .collect::<Vec<_>>(),
            ["selection", "selection", "portal", "portal", "category"]
        );
    }

    #[test]
    fn category_dependent_options_are_derived_from_each_runtime_document() {
        let first = resolve_semantic_listing_values(
            json!({"size":"43"}),
            &attributes(430),
            &json!({}),
            &json!({}),
        )
        .unwrap();
        let second = resolve_semantic_listing_values(
            json!({"size":"43"}),
            &attributes(9943),
            &json!({}),
            &json!({}),
        )
        .unwrap();

        assert_eq!(first.input["item_attributes"][0]["ids"], json!([430]));
        assert_eq!(second.input["item_attributes"][0]["ids"], json!([9943]));
    }

    #[test]
    fn ambiguous_unavailable_and_conflicting_values_are_structured_issues() {
        let result = resolve_semantic_listing_values(
            json!({
                "condition":"fair",
                "colors":["purple"],
                "package_size":"medium",
                "package_size_id":9
            }),
            &json!({"attributes":[{"code":"condition","options":[
                {"id":1,"title":"Fair","code":"fair"},
                {"id":2,"title":"Fair","code":"fair"}
            ]}]}),
            &json!({"colors":[{"id":3,"title":"Black"}]}),
            &json!({"package_sizes":[{"id":2,"title":"Medium","code":"medium"}]}),
        )
        .unwrap();

        assert_eq!(
            result
                .issues
                .iter()
                .map(|issue| issue.code.as_str())
                .collect::<Vec<_>>(),
            [
                "semantic_ambiguous",
                "semantic_unavailable",
                "semantic_conflict"
            ]
        );
        assert_eq!(result.issues[0].raw.as_ref().unwrap()["scope"], "selection");
        assert_eq!(result.issues[1].raw.as_ref().unwrap()["scope"], "portal");
    }

    #[test]
    fn matching_explicit_ids_remain_supported() {
        let result = resolve_semantic_listing_values(
            json!({
                "size":"43",
                "item_attributes":[{"code":"size","ids":[430]}],
                "colors":"black",
                "color_ids":[3],
                "package_size":"medium",
                "package_size_id":2
            }),
            &attributes(430),
            &json!({"colors":[{"id":3,"title":"Black"}]}),
            &json!({"package_sizes":[{"id":2,"title":"Medium"}]}),
        )
        .unwrap();
        assert!(result.issues.is_empty());
    }
}
