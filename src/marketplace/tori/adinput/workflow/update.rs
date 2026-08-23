use super::DraftWorkflow;
use crate::marketplace::tori::adinput::{
    adapter::AdInputApi,
    fields::{ordered_field_mutations, requested_sale_price},
    recovery::{UpdateResult, WorkflowError},
    validation::delivery_values,
};
use serde_json::{Map, Value};

impl<A: AdInputApi> DraftWorkflow<A> {
    pub async fn update(
        &self,
        draft_id: &str,
        patch: &Map<String, Value>,
    ) -> Result<UpdateResult, WorkflowError> {
        let current = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut completed = vec!["fetch_draft".to_owned()];
        let mut requested_values = current.values.clone();
        requested_values.extend(patch.clone());
        requested_values.remove("delivery");
        if patch.contains_key("price") {
            requested_sale_price(&requested_values)
                .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        }
        let requested_delivery = patch
            .get("delivery")
            .and_then(delivery_values)
            .unwrap_or_default();
        let applied = self
            .apply_field_mutations(
                current.clone(),
                ordered_field_mutations(patch.clone()),
                &mut completed,
                "draft_update",
                None,
            )
            .await?;
        let mut requested_fields = patch
            .iter()
            .flat_map(|(key, value)| {
                if key == "attributes" {
                    value
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(attribute, _)| format!("attributes.{attribute}"))
                        .collect::<Vec<_>>()
                } else {
                    vec![key.clone()]
                }
            })
            .collect::<Vec<_>>();
        requested_fields.sort();
        Ok(UpdateResult {
            etag_changed: applied.draft.etag != current.etag,
            draft: applied.draft,
            requested_fields,
            requested_delivery,
            persisted_fields: applied.progress.persisted,
            ignored_fields: applied.progress.absent,
            completed_steps: completed,
            warnings: applied.warnings,
        })
    }

    pub async fn delete(&self, draft_id: &str) -> Result<(), WorkflowError> {
        self.api
            .delete_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, false))
    }
}
