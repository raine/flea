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

    pub fn from_legacy_value(mut data: Value) -> Self {
        let next_actions = data
            .as_object_mut()
            .and_then(|object| object.remove("_next_actions"))
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let observation = data
            .as_object_mut()
            .and_then(|object| object.remove("_observation"))
            .and_then(|value| serde_json::from_value(value).ok());
        let warnings = data
            .get("warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|message| Warning {
                code: legacy_warning_code(message).to_owned(),
                message: message.to_owned(),
            })
            .collect();
        Self {
            data,
            next_actions,
            observation,
            warnings,
        }
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

fn legacy_warning_code(message: &str) -> &'static str {
    if message.starts_with("Tori returned an unrecognized successful mutation response") {
        "mutation.response_model_drift"
    } else if message.starts_with("Tori returned an ambiguous mutation response") {
        "mutation.observed_success"
    } else {
        "workflow.best_effort_failed"
    }
}
