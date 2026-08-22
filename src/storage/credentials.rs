use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub user_id: String,
    pub device_id: String,
}
