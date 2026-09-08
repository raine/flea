use std::{future::Future, pin::Pin};

use serde_json::{Map, Value, json};

use crate::{
    domain::{
        envelope::NextAction, vinted_listing::VintedListingDetail,
        vinted_listing_changes::VintedListingChanges,
    },
    error::{AppError, ExitClass},
    marketplace::{
        PortalId,
        vinted::{
            auth::VintedCredentialRecord,
            item::{VintedItemSession, validate_item_id},
            listing::{
                ListingLookup, VintedListingApi, VintedListings, normalize_detail, normalize_state,
                response_item,
            },
            listing_edit_payload::{prepare_update, project_editable},
            publication_discovery::VintedPublicationDiscoveryApi,
        },
    },
};

pub trait VintedListingEditApi: Send + Sync {
    fn update<'a>(
        &'a self,
        item_id: &'a str,
        body: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;
}

pub struct VintedListingEdits<'a> {
    session: &'a dyn VintedItemSession,
    api: &'a dyn VintedListingApi,
    edit_api: &'a dyn VintedListingEditApi,
    discovery_api: Option<&'a dyn VintedPublicationDiscoveryApi>,
}

impl<'a> VintedListingEdits<'a> {
    pub fn new(
        session: &'a dyn VintedItemSession,
        api: &'a dyn VintedListingApi,
        edit_api: &'a dyn VintedListingEditApi,
    ) -> Self {
        Self {
            session,
            api,
            edit_api,
            discovery_api: None,
        }
    }

    pub fn with_discovery(mut self, api: &'a dyn VintedPublicationDiscoveryApi) -> Self {
        self.discovery_api = Some(api);
        self
    }

    pub async fn update(
        &self,
        portal: PortalId,
        item_id: &str,
        changes: VintedListingChanges,
    ) -> Result<VintedListingDetail, AppError> {
        validate_item_id(item_id)?;
        validate_changes(&changes)?;

        let credentials = self.session.credentials(portal).await?;
        let wardrobe = self
            .api
            .wardrobe_item(&credentials, item_id)
            .await
            .and_then(|lookup| owned_wardrobe(&credentials, item_id, lookup))?;
        if response_item(&wardrobe)?
            .get("can_edit")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(unsupported_listing(
                item_id,
                "listing is not editable",
                None,
            ));
        }
        require_public_edit_state(item_id, &wardrobe)?;
        let editable = self.api.item_for_edit(&credentials, item_id).await?;
        validate_editable(item_id, &editable)?;

        let current = project_editable(item_id, &editable)?;
        let prepared = prepare_update(item_id, &editable, &changes)?;
        let before = normalize_detail(item_id, &wardrobe, &editable)?;

        if prepared.expected == current {
            return self.normalize_result(&credentials, before).await;
        }

        let mutation = match self.edit_api.update(item_id, prepared.body).await {
            Ok(response) => response,
            Err(error) if mutation_confirmed_rejected(&error) || mutation_not_attempted(&error) => {
                return Err(error);
            }
            Err(error) => return Err(uncertain_mutation(error, portal, item_id)),
        };
        if mutation_item_id(&mutation).as_deref() != Some(item_id) {
            return Err(uncertain_mutation(
                invalid_response("update response returned a missing or different item ID"),
                portal,
                item_id,
            ));
        }

        let mut verified = self
            .verify_update(&credentials, item_id, &changes, &prepared.expected, &before)
            .await;
        for _ in 0..5 {
            if !matches!(&verified, Err(error) if error.code == "vinted_listing.update_processing")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            verified = self
                .verify_update(&credentials, item_id, &changes, &prepared.expected, &before)
                .await;
        }
        match verified {
            Ok(detail) => self.normalize_result(&credentials, detail).await,
            Err(error) => Err(verification_failed(error, portal, item_id)),
        }
    }

    async fn verify_update(
        &self,
        credentials: &VintedCredentialRecord,
        item_id: &str,
        changes: &VintedListingChanges,
        expected: &Value,
        before: &VintedListingDetail,
    ) -> Result<VintedListingDetail, AppError> {
        let wardrobe = self
            .api
            .wardrobe_item(credentials, item_id)
            .await
            .and_then(|lookup| owned_wardrobe(credentials, item_id, lookup))?;
        if response_item(&wardrobe)?
            .get("is_processing")
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Err(AppError::validation(
                "vinted_listing.update_processing",
                "Vinted is still processing the accepted listing update",
            ));
        }
        let editable = self.api.item_for_edit(credentials, item_id).await?;
        validate_editable(item_id, &editable)?;

        let observed = project_editable(item_id, &editable)?;
        if &observed != expected {
            return Err(invalid_response(
                "post-update editable state did not match the submitted canonical state",
            ));
        }

        let detail = normalize_detail(item_id, &wardrobe, &editable)?;
        if !changed_scalars_match(&detail, changes) {
            return Err(invalid_response(
                "post-update listing detail did not contain the requested values",
            ));
        }
        if photo_identities(&detail) != photo_identities(before) {
            return Err(invalid_response(
                "post-update listing detail did not preserve the complete photo order",
            ));
        }
        Ok(detail)
    }

    async fn normalize_result(
        &self,
        credentials: &VintedCredentialRecord,
        mut detail: VintedListingDetail,
    ) -> Result<VintedListingDetail, AppError> {
        if let Some(discovery) = self.discovery_api {
            VintedListings::new(self.session, self.api)
                .with_discovery(discovery)
                .normalize_condition_identity(credentials, &mut detail)
                .await;
        }
        Ok(detail)
    }
}

fn validate_changes(changes: &VintedListingChanges) -> Result<(), AppError> {
    VintedListingChanges::new(
        changes.title.clone(),
        changes.description.clone(),
        changes.price.clone(),
    )
    .map(|_| ())
}

fn owned_wardrobe(
    credentials: &VintedCredentialRecord,
    item_id: &str,
    lookup: ListingLookup,
) -> Result<Value, AppError> {
    let raw = match lookup {
        ListingLookup::Found(raw) => raw,
        ListingLookup::Missing => {
            return Err(unsupported_listing(item_id, "listing is missing", None));
        }
        ListingLookup::Deleted => {
            return Err(unsupported_listing(item_id, "listing is deleted", None));
        }
    };
    let item = response_item(&raw)?;
    require_item_id(item_id, item, "wardrobe")?;

    let owner_id = identifier(item.get("user_id"));
    if owner_id.as_deref() != Some(credentials.user_id.as_str()) {
        return Err(AppError::validation(
            "vinted_listing.update_not_owned",
            "Vinted listing update supports only listings owned by the authenticated account",
        )
        .with_details(json!({ "item_id": item_id })));
    }
    if let Some(embedded_user) = item.get("user").filter(|user| !user.is_null())
        && embedded_user
            .as_object()
            .and_then(|user| identifier(user.get("id")))
            .as_deref()
            != Some(credentials.user_id.as_str())
    {
        return Err(AppError::validation(
            "vinted_listing.update_not_owned",
            "Vinted listing ownership signals did not agree",
        )
        .with_details(json!({ "item_id": item_id })));
    }
    Ok(raw)
}

fn require_public_edit_state(item_id: &str, raw: &Value) -> Result<(), AppError> {
    let item = response_item(raw)?;
    let state = normalize_state(item);
    let inactive_flag = ["is_draft", "is_closed", "is_hidden", "is_processing"]
        .into_iter()
        .any(|field| item.get(field).and_then(Value::as_bool) != Some(false));
    if state != crate::domain::vinted_listing::VintedListingState::Public || inactive_flag {
        return Err(unsupported_listing(
            item_id,
            "listing is not in an eligible public state",
            Some(state),
        ));
    }
    Ok(())
}

fn validate_editable(item_id: &str, raw: &Value) -> Result<(), AppError> {
    let item = response_item(raw)?;
    require_item_id(item_id, item, "editable listing")?;
    if item.get("is_draft").and_then(Value::as_bool) != Some(false) {
        return Err(unsupported_listing(
            item_id,
            "editable source is not a published listing",
            None,
        ));
    }
    Ok(())
}

fn require_item_id(
    item_id: &str,
    item: &Map<String, Value>,
    observation: &str,
) -> Result<(), AppError> {
    if identifier(item.get("id")).as_deref() != Some(item_id) {
        return Err(invalid_response(&format!(
            "{observation} returned a missing or different item ID"
        )));
    }
    Ok(())
}

fn mutation_item_id(raw: &Value) -> Option<String> {
    let body = raw.get("data").unwrap_or(raw);
    body.pointer("/item/id").and_then(value_id)
}

fn identifier(value: Option<&Value>) -> Option<String> {
    value.and_then(value_id)
}

fn value_id(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn changed_scalars_match(detail: &VintedListingDetail, changes: &VintedListingChanges) -> bool {
    changes
        .title
        .as_deref()
        .is_none_or(|title| detail.title.as_deref() == Some(title))
        && changes
            .description
            .as_deref()
            .is_none_or(|description| detail.description.as_deref() == Some(description))
        && changes.price.as_deref().is_none_or(|price| {
            let expected = price.parse::<f64>().ok();
            let actual = detail
                .price
                .as_ref()
                .and_then(|price| price.amount.as_f64());
            expected
                .zip(actual)
                .is_some_and(|(expected, actual)| (expected - actual).abs() <= f64::EPSILON)
        })
}

fn photo_identities(detail: &VintedListingDetail) -> Vec<(usize, Option<&str>)> {
    detail
        .photos
        .iter()
        .map(|photo| (photo.order, photo.id.as_deref()))
        .collect()
}

fn unsupported_listing(
    item_id: &str,
    reason: &str,
    state: Option<crate::domain::vinted_listing::VintedListingState>,
) -> AppError {
    AppError::new(
        "vinted_listing.update_unsupported",
        "Vinted listing update supports only owned, editable public listings",
        ExitClass::Validation,
    )
    .with_details(json!({
        "item_id": item_id,
        "reason": reason,
        "state": state,
    }))
}

fn invalid_response(reason: &str) -> AppError {
    AppError::upstream(
        "vinted_listing.update_unexpected_response",
        "Vinted returned an unsupported listing update response",
    )
    .with_details(json!({ "reason": reason }))
}

fn mutation_confirmed_rejected(error: &AppError) -> bool {
    error
        .details
        .as_deref()
        .and_then(|details| details.get("http_status"))
        .and_then(Value::as_u64)
        .is_some_and(|status| (400..500).contains(&status))
        && error
            .details
            .as_deref()
            .and_then(|details| details.get("mutation_response"))
            .is_none()
}

fn mutation_not_attempted(error: &AppError) -> bool {
    let details = error.details.as_deref();
    if details
        .and_then(|details| details.get("mutation_attempted"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return true;
    }
    details
        .and_then(|details| details.get("transport_phase"))
        .and_then(Value::as_str)
        == Some("request")
        && details
            .and_then(|details| details.get("transport_kind"))
            .and_then(Value::as_str)
            == Some("connection")
}

fn uncertain_mutation(mut error: AppError, portal: PortalId, item_id: &str) -> AppError {
    error.code = "mutation.uncertain".to_owned();
    error.message =
        "The Vinted listing update outcome is uncertain; inspect the listing before any retry"
            .to_owned();
    error.safe_to_retry = false;
    error = error.with_partial(json!({
        "operation": "update",
        "item_id": item_id,
        "may_have_changed": true,
    }));
    error.next_actions = Box::new(vec![listing_show_action(portal, item_id)]);
    error
}

fn verification_failed(mut error: AppError, portal: PortalId, item_id: &str) -> AppError {
    let mut details = error
        .details
        .take()
        .map(|value| *value)
        .unwrap_or_else(|| json!({}));
    if let Some(details) = details.as_object_mut() {
        details.insert("cause_code".to_owned(), json!(error.code));
        details.insert("cause_message".to_owned(), json!(error.message));
    }
    error.details = Some(Box::new(details));
    error.code = "vinted_listing.update_verification_failed".to_owned();
    error.message =
        "Vinted accepted the listing update, but authoritative verification failed".to_owned();
    error.safe_to_retry = false;
    error = error.with_partial(json!({
        "operation": "update",
        "item_id": item_id,
        "may_have_changed": true,
    }));
    error.next_actions = Box::new(vec![listing_show_action(portal, item_id)]);
    error
}

fn listing_show_action(portal: PortalId, item_id: &str) -> NextAction {
    NextAction {
        command: format!("flea vinted --portal {portal} listing show {item_id}"),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;
    use crate::marketplace::vinted::auth::VintedCredentialRecord;

    struct ListingApi {
        wardrobes: Mutex<VecDeque<Result<ListingLookup, AppError>>>,
        editables: Mutex<VecDeque<Result<Value, AppError>>>,
        wardrobe_calls: Mutex<usize>,
        editable_calls: Mutex<usize>,
    }

    impl ListingApi {
        fn new(
            wardrobes: Vec<Result<ListingLookup, AppError>>,
            editables: Vec<Result<Value, AppError>>,
        ) -> Self {
            Self {
                wardrobes: Mutex::new(wardrobes.into()),
                editables: Mutex::new(editables.into()),
                wardrobe_calls: Mutex::new(0),
                editable_calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> (usize, usize) {
            (
                *self.wardrobe_calls.lock().unwrap(),
                *self.editable_calls.lock().unwrap(),
            )
        }
    }

    impl VintedListingApi for ListingApi {
        fn wardrobe_item<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _item_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<ListingLookup, AppError>> + Send + 'a>> {
            *self.wardrobe_calls.lock().unwrap() += 1;
            let result = self.wardrobes.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { result })
        }

        fn item_for_edit<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _item_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            *self.editable_calls.lock().unwrap() += 1;
            let result = self.editables.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { result })
        }

        fn wardrobe_items<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _condition: &'a str,
            _page: usize,
            _per_page: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            panic!("listing edits do not scan wardrobe collections")
        }
    }

    struct EditApi {
        results: Mutex<VecDeque<Result<Value, AppError>>>,
        bodies: Mutex<Vec<Value>>,
    }

    impl EditApi {
        fn succeeding() -> Self {
            Self {
                results: Mutex::new(vec![Ok(json!({"item":{"id":"9001"}}))].into()),
                bodies: Mutex::new(Vec::new()),
            }
        }

        fn with_result(result: Result<Value, AppError>) -> Self {
            Self {
                results: Mutex::new(vec![result].into()),
                bodies: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.bodies.lock().unwrap().len()
        }
    }

    impl VintedListingEditApi for EditApi {
        fn update<'a>(
            &'a self,
            _item_id: &'a str,
            body: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.bodies.lock().unwrap().push(body);
            let result = self.results.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { result })
        }
    }

    fn credentials() -> VintedCredentialRecord {
        VintedCredentialRecord::new_for_adapter(
            PortalId::Fi,
            "owner-1".to_owned(),
            None,
            "access".to_owned(),
            "refresh".to_owned(),
            u64::MAX,
            "device".to_owned(),
            "anonymous".to_owned(),
            None,
        )
    }

    fn wardrobe(status: &str) -> Value {
        json!({
            "item": {
                "id": "9001",
                "title": "Bicycle lock",
                "can_edit": true,
                "user_id": "owner-1",
                "user": {"id": "owner-1"},
                "is_draft": false,
                "is_closed": false,
                "is_hidden": false,
                "is_processing": false,
                "status": status,
                "url": "/items/9001-bicycle-lock"
            }
        })
    }

    fn editable(title: &str) -> Value {
        json!({
            "item": {
                "id": "9001",
                "title": title,
                "description": "Two keys",
                "catalog_id": "3412",
                "brand_dto": {"id": "77", "title": "Kryptonite"},
                "brand_id": "77",
                "color1_id": "1",
                "color2_id": "3",
                "package_size_id": "2",
                "price": {"amount": "12.50", "currency_code": "EUR"},
                "currency": "EUR",
                "shipment_prices": {"domestic": null, "international": null},
                "domestic_shipment_price": null,
                "international_shipment_price": null,
                "photos": [{"id": 41, "orientation": 0}, {"id": "42", "orientation": 0}],
                "item_attributes": [{"code": "condition", "ids": [6]}],
                "is_draft": false,
                "is_unisex": false,
                "isbn": null,
                "measurement_length": null,
                "measurement_width": null,
                "manufacturer": null,
                "manufacturer_labelling": null,
                "model": null,
                "ai_photo": false
            },
            "parcel": {"width": 10.0, "height": 5.0, "length": 15.0, "weight": 0.4}
        })
    }

    fn title_change(title: &str) -> VintedListingChanges {
        VintedListingChanges {
            title: Some(title.to_owned()),
            description: None,
            price: None,
        }
    }

    #[tokio::test]
    async fn validates_programmatic_changes_before_remote_reads() {
        let session = |_| -> Result<VintedCredentialRecord, AppError> {
            panic!("invalid changes must not acquire credentials")
        };
        let api = ListingApi::new(Vec::new(), Vec::new());
        let edit_api = EditApi::succeeding();

        for changes in [
            VintedListingChanges {
                title: None,
                description: None,
                price: None,
            },
            VintedListingChanges {
                title: Some("  ".to_owned()),
                description: None,
                price: None,
            },
            VintedListingChanges {
                title: None,
                description: None,
                price: Some("NaN".to_owned()),
            },
        ] {
            let error = VintedListingEdits::new(&session, &api, &edit_api)
                .update(PortalId::Fi, "9001", changes)
                .await
                .unwrap_err();
            assert!(matches!(
                error.exit_class,
                ExitClass::Usage | ExitClass::Validation
            ));
        }
        assert_eq!(api.calls(), (0, 0));
        assert_eq!(edit_api.calls(), 0);
    }

    #[tokio::test]
    async fn rejects_invalid_item_id_before_remote_reads() {
        let session = |_| -> Result<VintedCredentialRecord, AppError> {
            panic!("invalid IDs must not acquire credentials")
        };
        let api = ListingApi::new(Vec::new(), Vec::new());
        let edit_api = EditApi::succeeding();

        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "not-an-id", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted_item.invalid_id");
        assert_eq!(api.calls(), (0, 0));
        assert_eq!(edit_api.calls(), 0);
    }

    #[tokio::test]
    async fn rejects_ineligible_or_unowned_wardrobe_state_without_mutating() {
        let session = |_| Ok(credentials());
        let mut cases = Vec::new();
        for status in ["draft", "hidden", "sold", "processing", "deleted"] {
            cases.push(wardrobe(status));
        }
        let mut not_editable = wardrobe("active");
        not_editable["item"]["can_edit"] = json!(false);
        cases.push(not_editable);
        let mut not_owned = wardrobe("active");
        not_owned["item"]["user_id"] = json!("someone-else");
        cases.push(not_owned);

        for raw in cases {
            let api = ListingApi::new(vec![Ok(ListingLookup::Found(raw))], Vec::new());
            let edit_api = EditApi::succeeding();
            let error = VintedListingEdits::new(&session, &api, &edit_api)
                .update(PortalId::Fi, "9001", title_change("Safer lock"))
                .await
                .unwrap_err();

            assert_eq!(error.exit_class, ExitClass::Validation);
            assert_eq!(api.calls(), (1, 0));
            assert_eq!(edit_api.calls(), 0);
        }
    }

    #[tokio::test]
    async fn rejects_absent_and_invalid_editable_sources_without_mutating() {
        let session = |_| Ok(credentials());
        for lookup in [ListingLookup::Missing, ListingLookup::Deleted] {
            let api = ListingApi::new(vec![Ok(lookup)], Vec::new());
            let edit_api = EditApi::succeeding();

            let error = VintedListingEdits::new(&session, &api, &edit_api)
                .update(PortalId::Fi, "9001", title_change("Safer lock"))
                .await
                .unwrap_err();

            assert_eq!(error.code, "vinted_listing.update_unsupported");
            assert_eq!(api.calls(), (1, 0));
            assert_eq!(edit_api.calls(), 0);
        }

        let mut draft = editable("Bicycle lock");
        draft["item"]["is_draft"] = json!(true);
        let api = ListingApi::new(
            vec![Ok(ListingLookup::Found(wardrobe("active")))],
            vec![Ok(draft)],
        );
        let edit_api = EditApi::succeeding();
        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted_listing.update_unsupported");
        assert_eq!(api.calls(), (1, 1));
        assert_eq!(edit_api.calls(), 0);

        let mut incomplete = editable("Bicycle lock");
        incomplete["item"]
            .as_object_mut()
            .unwrap()
            .remove("item_attributes");
        let api = ListingApi::new(
            vec![Ok(ListingLookup::Found(wardrobe("active")))],
            vec![Ok(incomplete)],
        );
        let edit_api = EditApi::succeeding();
        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted_listing_edit.unsupported_editable_state");
        assert_eq!(api.calls(), (1, 1));
        assert_eq!(edit_api.calls(), 0);
    }

    #[tokio::test]
    async fn submits_once_and_verifies_both_authoritative_observations() {
        let session = |_| Ok(credentials());
        let api = ListingApi::new(
            vec![
                Ok(ListingLookup::Found(wardrobe("active"))),
                Ok(ListingLookup::Found(wardrobe("active"))),
            ],
            vec![Ok(editable("Bicycle lock")), Ok(editable("Safer lock"))],
        );
        let edit_api = EditApi::succeeding();

        let detail = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap();

        assert_eq!(detail.title.as_deref(), Some("Safer lock"));
        assert_eq!(detail.photos.len(), 2);
        assert_eq!(api.calls(), (2, 2));
        assert_eq!(edit_api.calls(), 1);
        let bodies = edit_api.bodies.lock().unwrap();
        assert_eq!(bodies[0]["item"]["description"], "Two keys");
        assert_eq!(
            bodies[0]["item"]["assigned_photos"],
            json!([{"id":"41","orientation":0},{"id":"42","orientation":0}])
        );
        assert_eq!(bodies[0]["item"]["update_photos"], 0);
    }

    #[tokio::test]
    async fn verifies_description_and_price_changes_with_preserved_currency() {
        let session = |_| Ok(credentials());
        let mut after = editable("Bicycle lock");
        after["item"]["description"] = json!("Three keys");
        after["item"]["price"]["amount"] = json!("15.00");
        let api = ListingApi::new(
            vec![
                Ok(ListingLookup::Found(wardrobe("active"))),
                Ok(ListingLookup::Found(wardrobe("active"))),
            ],
            vec![Ok(editable("Bicycle lock")), Ok(after)],
        );
        let edit_api = EditApi::succeeding();
        let changes = VintedListingChanges {
            title: None,
            description: Some("Three keys".to_owned()),
            price: Some("15.00".to_owned()),
        };

        let detail = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", changes)
            .await
            .unwrap();

        assert_eq!(detail.description.as_deref(), Some("Three keys"));
        assert_eq!(detail.price.as_ref().unwrap().amount, json!(15.0));
        assert_eq!(
            detail.price.as_ref().unwrap().currency.as_deref(),
            Some("EUR")
        );
        let bodies = edit_api.bodies.lock().unwrap();
        assert_eq!(bodies[0]["item"]["price"], "15.0");
        assert_eq!(bodies[0]["item"]["currency"], "EUR");
        drop(bodies);
        assert_eq!(edit_api.calls(), 1);
        assert_eq!(api.calls(), (2, 2));
    }

    #[tokio::test]
    async fn unchanged_authoritative_value_is_a_noop() {
        let session = |_| Ok(credentials());
        let api = ListingApi::new(
            vec![Ok(ListingLookup::Found(wardrobe("active")))],
            vec![Ok(editable("Bicycle lock"))],
        );
        let edit_api = EditApi::succeeding();

        let detail = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Bicycle lock"))
            .await
            .unwrap();

        assert_eq!(detail.title.as_deref(), Some("Bicycle lock"));
        assert_eq!(api.calls(), (1, 1));
        assert_eq!(edit_api.calls(), 0);
    }

    #[tokio::test]
    async fn ambiguous_mutation_is_not_retried_and_requires_listing_inspection() {
        let session = |_| Ok(credentials());
        let api = ListingApi::new(
            vec![Ok(ListingLookup::Found(wardrobe("active")))],
            vec![Ok(editable("Bicycle lock"))],
        );
        let edit_api = EditApi::with_result(Err(AppError::upstream(
            "fixture.transport_failed",
            "transport failed",
        )));

        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "mutation.uncertain");
        assert_eq!(error.exit_class, ExitClass::Partial);
        assert!(!error.safe_to_retry);
        assert_eq!(error.partial.as_ref().unwrap()["operation"], "update");
        assert_eq!(error.partial.as_ref().unwrap()["may_have_changed"], true);
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi listing show 9001"
        );
        assert_eq!(edit_api.calls(), 1);
        assert_eq!(api.calls(), (1, 1));
    }

    #[tokio::test]
    async fn marked_preflight_error_preserves_original_guidance_without_uncertainty() {
        let session = |_| Ok(credentials());
        let api = ListingApi::new(
            vec![Ok(ListingLookup::Found(wardrobe("active")))],
            vec![Ok(editable("Bicycle lock"))],
        );
        let mut preflight = AppError::authentication(
            "vinted.web_csrf_unavailable",
            "Browser security token is unavailable",
        )
        .with_details(json!({"mutation_attempted": false}));
        preflight.safe_to_retry = true;
        preflight.next_actions.push(NextAction {
            command: "flea vinted --portal fi auth status --browser".to_owned(),
        });
        let edit_api = EditApi::with_result(Err(preflight));

        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted.web_csrf_unavailable");
        assert_eq!(error.exit_class, ExitClass::Authentication);
        assert!(error.safe_to_retry);
        assert!(error.partial.is_none());
        assert_eq!(error.details.as_ref().unwrap()["mutation_attempted"], false);
        assert_eq!(error.next_actions.len(), 1);
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi auth status --browser"
        );
        assert_eq!(edit_api.calls(), 1);
        assert_eq!(api.calls(), (1, 1));
    }

    #[tokio::test]
    async fn processing_verification_retries_reads_without_replaying_the_write() {
        for completes in [true, false] {
            let session = |_| Ok(credentials());
            let mut processing = wardrobe("active");
            processing["item"]["can_edit"] = json!(false);
            processing["item"]["is_processing"] = json!(true);
            let mut reads = vec![Ok(ListingLookup::Found(wardrobe("active")))];
            reads.push(Ok(ListingLookup::Found(processing.clone())));
            let edits = if completes {
                reads.push(Ok(ListingLookup::Found(wardrobe("active"))));
                vec![Ok(editable("Bicycle lock")), Ok(editable("Safer lock"))]
            } else {
                reads.extend((0..5).map(|_| Ok(ListingLookup::Found(processing.clone()))));
                vec![Ok(editable("Bicycle lock"))]
            };
            let api = ListingApi::new(reads, edits);
            let edit_api = EditApi::succeeding();
            let result = VintedListingEdits::new(&session, &api, &edit_api)
                .update(PortalId::Fi, "9001", title_change("Safer lock"))
                .await;
            if completes {
                assert!(result.is_ok());
                assert_eq!(api.calls(), (3, 2));
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, "vinted_listing.update_verification_failed");
                assert!(!error.safe_to_retry);
                assert_eq!(api.calls(), (7, 1));
            }
            assert_eq!(edit_api.calls(), 1);
        }
    }

    #[tokio::test]
    async fn verification_does_not_require_permission_for_another_edit() {
        let session = |_| Ok(credentials());
        let mut after = wardrobe("active");
        after["item"]["can_edit"] = json!(false);
        let api = ListingApi::new(
            vec![
                Ok(ListingLookup::Found(wardrobe("active"))),
                Ok(ListingLookup::Found(after)),
            ],
            vec![Ok(editable("Bicycle lock")), Ok(editable("Safer lock"))],
        );
        let edit_api = EditApi::succeeding();

        VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap();

        assert_eq!(edit_api.calls(), 1);
        assert_eq!(api.calls(), (2, 2));
    }

    #[tokio::test]
    async fn changed_photo_orientation_fails_canonical_verification() {
        let session = |_| Ok(credentials());
        let mut after = editable("Safer lock");
        after["item"]["photos"][0]["orientation"] = json!(90);
        let api = ListingApi::new(
            vec![
                Ok(ListingLookup::Found(wardrobe("active"))),
                Ok(ListingLookup::Found(wardrobe("active"))),
            ],
            vec![Ok(editable("Bicycle lock")), Ok(after)],
        );
        let edit_api = EditApi::succeeding();

        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted_listing.update_verification_failed");
        assert_eq!(
            error.details.as_ref().unwrap()["reason"],
            "post-update editable state did not match the submitted canonical state"
        );
        assert!(!error.safe_to_retry);
        assert_eq!(error.partial.as_ref().unwrap()["may_have_changed"], true);
        assert_eq!(edit_api.calls(), 1);
        assert_eq!(api.calls(), (2, 2));
    }

    #[tokio::test]
    async fn confirmed_rejection_remains_distinct_and_is_not_retried() {
        let session = |_| Ok(credentials());
        let api = ListingApi::new(
            vec![Ok(ListingLookup::Found(wardrobe("active")))],
            vec![Ok(editable("Bicycle lock"))],
        );
        let edit_api = EditApi::with_result(Err(AppError::validation(
            "vinted.publication_validation_failed",
            "invalid title",
        )
        .with_details(json!({"http_status": 422, "field_errors": {"title": "invalid"}}))));

        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted.publication_validation_failed");
        assert_eq!(error.exit_class, ExitClass::Validation);
        assert!(error.partial.is_none());
        assert_eq!(edit_api.calls(), 1);
        assert_eq!(api.calls(), (1, 1));
    }

    #[tokio::test]
    async fn mismatched_success_response_is_an_uncertain_single_write() {
        let session = |_| Ok(credentials());
        let api = ListingApi::new(
            vec![Ok(ListingLookup::Found(wardrobe("active")))],
            vec![Ok(editable("Bicycle lock"))],
        );
        let edit_api = EditApi::with_result(Ok(json!({"item":{"id":"9002"}})));

        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "mutation.uncertain");
        assert!(!error.safe_to_retry);
        assert_eq!(error.partial.as_ref().unwrap()["may_have_changed"], true);
        assert_eq!(edit_api.calls(), 1);
        assert_eq!(api.calls(), (1, 1));
    }

    #[tokio::test]
    async fn failed_post_write_verification_is_nonretryable() {
        let session = |_| Ok(credentials());
        let api = ListingApi::new(
            vec![
                Ok(ListingLookup::Found(wardrobe("active"))),
                Ok(ListingLookup::Found(wardrobe("active"))),
            ],
            vec![Ok(editable("Bicycle lock")), Ok(editable("Bicycle lock"))],
        );
        let edit_api = EditApi::succeeding();

        let error = VintedListingEdits::new(&session, &api, &edit_api)
            .update(PortalId::Fi, "9001", title_change("Safer lock"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted_listing.update_verification_failed");
        assert_eq!(error.exit_class, ExitClass::Partial);
        assert!(!error.safe_to_retry);
        assert_eq!(
            error.details.as_ref().unwrap()["cause_code"],
            "vinted_listing.update_unexpected_response"
        );
        assert_eq!(error.partial.as_ref().unwrap()["item_id"], "9001");
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi listing show 9001"
        );
        assert_eq!(edit_api.calls(), 1);
        assert_eq!(api.calls(), (2, 2));
    }
}
