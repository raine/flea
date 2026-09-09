use std::{future::Future, pin::Pin, sync::{Arc, Mutex}};

use flea::{AppError, PortalId, dependencies::{ApplicationDependencies, VintedCredentialRecord, VintedSalesApi, VintedSalesRequest}, domain::vinted_sale::VintedSalesStatus, run_with_dependencies};
use serde_json::{Value, json};

#[derive(Default)]
struct FixtureApi { calls: Mutex<Vec<VintedSalesRequest>> }

impl VintedSalesApi for FixtureApi {
    fn orders<'a>(&'a self, _: &'a VintedCredentialRecord, request: &'a VintedSalesRequest) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        self.calls.lock().unwrap().push(request.clone());
        let text = match request.page {
            1 => include_str!("fixtures/vinted/sales/page-1.json"),
            2 => include_str!("fixtures/vinted/sales/page-2.json"),
            _ => panic!("unexpected page"),
        };
        Box::pin(async move { Ok(serde_json::from_str(text).unwrap()) })
    }
}

fn credentials() -> VintedCredentialRecord {
    VintedCredentialRecord::new_for_adapter(PortalId::Fi, "private-account".into(), Some("private-login".into()), "private-access".into(), "private-refresh".into(), u64::MAX, "private-device".into(), "private-anonymous".into(), None)
}

fn setup() -> (ApplicationDependencies, Arc<FixtureApi>) {
    let api = Arc::new(FixtureApi::default());
    let dependencies = ApplicationDependencies::production()
        .with_vinted_credentials_provider(|_| Ok(credentials()))
        .with_vinted_sales_api(api.clone());
    (dependencies, api)
}

#[test]
fn cli_defaults_and_pagination_preserve_transaction_identity_and_redact_extras() {
    let (dependencies, api) = setup();
    let result = run_with_dependencies(["flea", "--format", "json", "vinted", "sales", "list"], &dependencies);
    assert_eq!(result.exit_code, 0, "{}", result.document);
    let envelope: Value = serde_json::from_str(&result.document).unwrap();
    assert_eq!(envelope["data"]["status"], "completed");
    assert_eq!(envelope["data"]["count"], 2);
    assert_eq!(envelope["data"]["limit"], 20);
    assert_eq!(envelope["data"]["sales"][0]["transaction_id"], "7001");
    assert_eq!(envelope["data"]["sales"][0]["conversation_id"], "8001");
    assert_eq!(envelope["data"]["sales"][0]["price"]["amount"], 12.5);
    assert_eq!(envelope["data"]["sales"][1]["transaction_id"], Value::Null);
    assert_eq!(envelope["data"]["sales"][1]["transaction_user_status"], "future_status");
    assert_eq!(envelope["data"]["next_page"], 2);
    assert_eq!(envelope["next_actions"][0]["command"], "flea vinted --portal fi sales list --status completed --page 2 --limit 20");
    for absent in ["private-", "listing_id", "item_id", "canonical_url", "completed_at", "earnings"] { assert!(!result.document.contains(absent), "unexpected field: {absent}"); }
    assert_eq!(*api.calls.lock().unwrap(), vec![VintedSalesRequest::default()]);

    let result = run_with_dependencies(["flea", "--format", "json", "vinted", "sales", "list", "--page", "2", "--limit", "2", "--status", "all"], &dependencies);
    assert_eq!(result.exit_code, 0);
    let envelope: Value = serde_json::from_str(&result.document).unwrap();
    assert_eq!(envelope["data"]["pagination"]["current_page"], 2);
    assert_eq!(envelope["data"]["sales"][0]["transaction_id"], "7003");
    assert_eq!(envelope["data"]["next_page"], Value::Null);
    assert_eq!(envelope["next_actions"], json!([]));
    assert_eq!(api.calls.lock().unwrap()[1], VintedSalesRequest {status: VintedSalesStatus::All, page: 2, limit: 2});
}

#[test]
fn toon_and_all_status_filters_use_the_same_read_path() {
    for (status, expected) in [("all", VintedSalesStatus::All), ("in_progress", VintedSalesStatus::InProgress), ("completed", VintedSalesStatus::Completed), ("canceled", VintedSalesStatus::Canceled)] {
        let (dependencies, api) = setup();
        let result = run_with_dependencies(["flea", "--format", "toon", "vinted", "sales", "list", "--status", status], &dependencies);
        assert_eq!(result.exit_code, 0, "{}", result.document);
        assert!(result.document.contains("transaction_id"));
        assert!(result.document.contains("7001"));
        assert!(!result.document.contains("private-"));
        assert_eq!(api.calls.lock().unwrap()[0].status, expected);
    }
}

#[test]
fn invalid_inputs_do_not_access_credentials_or_orders() {
    let api = Arc::new(FixtureApi::default());
    let dependencies = ApplicationDependencies::production()
        .with_vinted_credentials_provider(|_| panic!("invalid input must fail before credentials"))
        .with_vinted_sales_api(api.clone());
    for args in [["--page", "0"], ["--page", "2147483648"], ["--limit", "0"], ["--limit", "97"], ["--status", "sold"], ["--status", "in-progress"]] {
        let result = run_with_dependencies(["flea", "--format", "json", "vinted", "sales", "list", args[0], args[1]], &dependencies);
        assert_eq!(result.exit_code, 2);
    }
    assert!(api.calls.lock().unwrap().is_empty());
}

#[test]
fn missing_auth_is_not_an_empty_success() {
    let api = Arc::new(FixtureApi::default());
    let dependencies = ApplicationDependencies::production()
        .with_vinted_credentials_provider(|_| Err(AppError::authentication("fixture.no_auth", "authentication required")))
        .with_vinted_sales_api(api.clone());
    let result = run_with_dependencies(["flea", "--format", "json", "vinted", "sales", "list"], &dependencies);
    assert_eq!(result.exit_code, 10);
    let envelope: Value = serde_json::from_str(&result.document).unwrap();
    assert_eq!(envelope["ok"], false);
    assert!(api.calls.lock().unwrap().is_empty());
}
