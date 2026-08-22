use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    time::Duration,
};

use reqwest::{
    Method as ReqwestMethod,
    header::{CONTENT_TYPE, HeaderValue, LOCATION},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use time::format_description::well_known::Rfc3339;

use crate::{
    api::client::{
        HttpError, MultipartPart, RequestSpec, ToriClient, TransportErrorKind, compatibility,
    },
    diagnostics,
    domain::{
        commerce::{
            TradeType, normalize_trade_type, normalized_select_to_machine, select_values_equal,
        },
        field::{
            Field, FieldStatus, FieldType, Requirement, UpstreamValidationError, ValidationIssue,
            map_validation_errors, stable_field_key,
        },
        observation::{Observation, ObservationOperation, ObservationState, StatusEvidence},
    },
    image_processing::{self, ImageProcessingReport, ProcessedImage, ProcessingError},
    retry::{FailureKind, OperationMethod, RetryClassification, RetryContext, classify},
};

mod adapter;
mod delivery;
mod fields;
mod http;
mod images;
mod normalization;
mod recovery;
mod types;
mod validation;
mod workflow;

pub use adapter::*;
pub use http::*;
pub use images::*;
pub(crate) use recovery::completed_steps_have_mutation;
pub use recovery::*;
pub use types::*;
pub use validation::*;
pub use workflow::*;
