use std::ops::{Deref, DerefMut};

use serde_json::Value;

use crate::domain::{
    envelope::{NextAction, Warning},
    observation::Observation,
};

#[derive(Debug, PartialEq)]
pub struct CommandOutcome {
    pub data: Value,
    pub next_actions: Vec<NextAction>,
    pub observation: Option<Observation>,
    pub warnings: Vec<Warning>,
}

impl CommandOutcome {
    pub fn new(data: Value) -> Self {
        Self {
            data,
            next_actions: Vec::new(),
            observation: None,
            warnings: Vec::new(),
        }
    }

    pub fn with_next_actions(mut self, next_actions: Vec<NextAction>) -> Self {
        self.next_actions = next_actions;
        self
    }

    pub fn with_observation(mut self, observation: Observation) -> Self {
        self.observation = Some(observation);
        self
    }

    pub fn with_warnings(mut self, warnings: Vec<Warning>) -> Self {
        self.warnings = warnings;
        self
    }
}

impl From<Value> for CommandOutcome {
    fn from(data: Value) -> Self {
        Self::new(data)
    }
}

impl Deref for CommandOutcome {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl DerefMut for CommandOutcome {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl PartialEq<Value> for CommandOutcome {
    fn eq(&self, other: &Value) -> bool {
        self.data == *other
    }
}
