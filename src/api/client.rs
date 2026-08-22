use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub request_timeout: Duration,
}

pub trait ToriClient: Send + Sync {}
