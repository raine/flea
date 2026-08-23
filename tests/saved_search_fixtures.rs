use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use flea::api::{
    client::{HttpError, HttpResponse, RequestBody, RequestSpec, ToriClient, compatibility},
    saved_searches::{
        CreateSavedSearch, HttpSavedSearchesApi, SavedSearchApiError, SavedSearches,
        SavedSearchesApi,
    },
};
use reqwest::StatusCode;
use serde_json::{Value, json};

#[derive(Clone)]
struct FixtureClient {
    requests: Arc<Mutex<Vec<RequestSpec>>>,
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, HttpError>>>>,
}

impl FixtureClient {
    fn new(responses: impl IntoIterator<Item = Result<HttpResponse, HttpError>>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
        }
    }
}

impl ToriClient for FixtureClient {
    fn execute(
        &self,
        request: RequestSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>> {
        self.requests.lock().unwrap().push(request);
        let response = self.responses.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { response })
    }
}

fn ready<T: Send + 'static>(value: T) -> Pin<Box<dyn Future<Output = T> + Send>> {
    Box::pin(async move { value })
}

fn response(status: StatusCode, body: impl Into<Vec<u8>>) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status,
        headers: Default::default(),
        body: body.into(),
    })
}

#[tokio::test]
async fn http_fixture_covers_list_show_create_update_and_delete_protocol() {
    let fixture = include_bytes!("fixtures/saved-searches/list.json");
    let client = FixtureClient::new([
        response(StatusCode::OK, fixture.as_slice()),
        response(StatusCode::OK, fixture.as_slice()),
        response(StatusCode::OK, b"987654321".as_slice()),
        response(StatusCode::NO_CONTENT, Vec::new()),
        response(StatusCode::NO_CONTENT, Vec::new()),
    ]);
    let api = HttpSavedSearchesApi::new(Arc::new(client.clone()));

    assert_eq!(api.list(Some(25)).await.unwrap().len(), 1);
    assert!(api.show("987654321").await.unwrap().is_some());
    let body = json!({"description":"fixture"});
    assert_eq!(api.create(&body).await.unwrap(), "987654321");
    api.update(&body).await.unwrap();
    api.delete("opaque/id").await.unwrap();

    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].path_and_query,
        "/public/search?type=alert&rows=25&clientId=ANDROID"
    );
    assert_eq!(
        requests[1].path_and_query,
        "/public/search?id=987654321&clientId=ANDROID"
    );
    assert_eq!(
        requests[2].path_and_query,
        "/public/search?clientId=ANDROID"
    );
    assert_eq!(
        requests[3].path_and_query,
        "/public/search?clientId=ANDROID"
    );
    assert_eq!(
        requests[4].path_and_query,
        "/public/search?id=opaque%2Fid&clientId=ANDROID"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.service == compatibility::SERVICE_SAVED_SEARCHES)
    );
    let RequestBody::Bytes(bytes) = &requests[2].body else {
        panic!("create JSON body")
    };
    assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), body);
}

#[tokio::test]
async fn fixture_normalizes_opaque_ids_values_and_parameters() {
    let raw: Vec<Value> =
        serde_json::from_slice(include_bytes!("fixtures/saved-searches/list.json")).unwrap();
    let api = StateApi::new(raw);
    let result = SavedSearches::new(&api).list(None).await.unwrap();

    assert_eq!(result[0].id, "987654321");
    assert_eq!(result[0].created.as_deref(), Some("1750000000456"));
    assert_eq!(result[0].notifications, ["EMAIL", "NC"]);
    assert_eq!(result[0].parameters["dealer_segment"], ["1"]);
}

struct StateApi {
    searches: Mutex<Vec<Value>>,
    create_error: Mutex<Option<SavedSearchApiError>>,
    last_create: Mutex<Option<Value>>,
    update_error: Mutex<Option<SavedSearchApiError>>,
    last_update: Mutex<Option<Value>>,
    delete_error: Mutex<Option<SavedSearchApiError>>,
}

impl StateApi {
    fn new(searches: Vec<Value>) -> Self {
        Self {
            searches: Mutex::new(searches),
            create_error: Mutex::new(None),
            last_create: Mutex::new(None),
            update_error: Mutex::new(None),
            last_update: Mutex::new(None),
            delete_error: Mutex::new(None),
        }
    }
}

impl SavedSearchesApi for StateApi {
    fn list(
        &self,
        _: Option<usize>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>, SavedSearchApiError>> + Send + '_>> {
        ready(Ok(self.searches.lock().unwrap().clone()))
    }

    fn show<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, SavedSearchApiError>> + Send + 'a>> {
        ready(Ok(self
            .searches
            .lock()
            .unwrap()
            .iter()
            .find(|value| value["id"].to_string().trim_matches('"') == id)
            .cloned()))
    }

    fn create<'a>(
        &'a self,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, SavedSearchApiError>> + Send + 'a>> {
        *self.last_create.lock().unwrap() = Some(body.clone());
        ready(Err(self
            .create_error
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(SavedSearchApiError::Transport)))
    }

    fn update<'a>(
        &'a self,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), SavedSearchApiError>> + Send + 'a>> {
        *self.last_update.lock().unwrap() = Some(body.clone());
        ready(Err(self
            .update_error
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(SavedSearchApiError::Transport)))
    }

    fn delete<'a>(
        &'a self,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SavedSearchApiError>> + Send + 'a>> {
        ready(Err(self
            .delete_error
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(SavedSearchApiError::Transport)))
    }
}

fn intended_input() -> CreateSavedSearch {
    CreateSavedSearch {
        name: "Helsinki chairs".to_owned(),
        notifications: vec!["EMAIL".to_owned(), "NC".to_owned()],
        parameters: BTreeMap::from([
            ("dealer_segment".to_owned(), vec!["1".to_owned()]),
            ("location".to_owned(), vec!["1.100018.110091".to_owned()]),
            ("q".to_owned(), vec!["tuoli".to_owned()]),
        ]),
    }
}

#[tokio::test]
async fn uncertain_create_recovers_existing_result_or_proves_retryable_absence() {
    let existing: Vec<Value> =
        serde_json::from_slice(include_bytes!("fixtures/saved-searches/list.json")).unwrap();
    let present_api = StateApi::new(existing);
    let recovered = SavedSearches::new(&present_api)
        .create(intended_input())
        .await
        .unwrap();
    assert_eq!(recovered.id, "987654321");
    let body = present_api.last_create.lock().unwrap().clone().unwrap();
    assert_eq!(body["type"], "alert");
    assert_eq!(body["searchKey"], "SEARCH_ID_BAP_COMMON");
    assert_eq!(body["notifications"], json!(["EMAIL", "NC"]));
    assert_eq!(body["parameters"]["q"], json!(["tuoli"]));
    assert!(body["id"].is_null());
    assert_eq!(body["deleted"], false);

    let absent_api = StateApi::new(Vec::new());
    let error = SavedSearches::new(&absent_api)
        .create(intended_input())
        .await
        .unwrap_err();
    assert_eq!(error.code, "mutation.uncertain");
    assert!(error.safe_to_retry);
    assert_eq!(error.next_actions[0].command, "flea tori saved-search list");
}

#[tokio::test]
async fn uncertain_update_and_delete_return_read_only_recovery_actions() {
    let existing: Vec<Value> =
        serde_json::from_slice(include_bytes!("fixtures/saved-searches/list.json")).unwrap();
    let api = StateApi::new(existing);
    let saved = SavedSearches::new(&api);

    let update = saved
        .update("987654321", Some("Changed".to_owned()), None)
        .await
        .unwrap_err();
    assert!(update.safe_to_retry);
    let body = api.last_update.lock().unwrap().clone().unwrap();
    assert_eq!(body["description"], "Changed");
    assert!(body["userKey"].is_null());
    assert!(body["vertical"].is_null());
    assert_eq!(body["parameters"]["dealer_segment"], json!(["1"]));
    assert_eq!(
        update.next_actions[0].command,
        "flea tori saved-search show 987654321"
    );

    let delete = saved.delete("987654321").await.unwrap_err();
    assert!(delete.safe_to_retry);
    assert_eq!(
        delete.next_actions[0].command,
        "flea tori saved-search show 987654321"
    );
}

#[tokio::test]
async fn transport_failure_without_recovery_evidence_is_unsafe() {
    struct Unavailable;
    impl SavedSearchesApi for Unavailable {
        fn list(
            &self,
            _: Option<usize>,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>, SavedSearchApiError>> + Send + '_>>
        {
            ready(Err(SavedSearchApiError::Transport))
        }

        fn show<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, SavedSearchApiError>> + Send + 'a>>
        {
            ready(Err(SavedSearchApiError::Transport))
        }

        fn create<'a>(
            &'a self,
            _: &'a Value,
        ) -> Pin<Box<dyn Future<Output = Result<String, SavedSearchApiError>> + Send + 'a>>
        {
            ready(Err(SavedSearchApiError::Transport))
        }

        fn update<'a>(
            &'a self,
            _: &'a Value,
        ) -> Pin<Box<dyn Future<Output = Result<(), SavedSearchApiError>> + Send + 'a>> {
            ready(Err(SavedSearchApiError::Transport))
        }

        fn delete<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), SavedSearchApiError>> + Send + 'a>> {
            ready(Err(SavedSearchApiError::Transport))
        }
    }
    let error = SavedSearches::new(&Unavailable)
        .create(intended_input())
        .await
        .unwrap_err();
    assert!(error.upstream_transient);
    assert!(!error.safe_to_retry);
}
