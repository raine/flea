use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthFlow {
    pub flow_id: String,
    pub expires_at_unix: u64,
}
