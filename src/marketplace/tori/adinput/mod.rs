mod adapter;
mod delivery;
mod fields;
mod http;
mod images;
mod input;
mod normalization;
mod recovery;
mod types;
mod validation;
mod workflow;

pub use adapter::{AdInputApi, HttpAdInputApi};
pub use http::{
    ApiError, ClientTransport, HttpRequest, HttpResponse, HttpTransport, Method, RequestBody,
    RetryPolicy,
};
pub use images::{PreparedImage, normalize_category, prepare_image};
pub use input::{
    DraftInput, DraftPreviewOutput, DraftPreviewResult, PreparedDraftInput, prepare, preview,
};
pub(crate) use recovery::completed_steps_have_mutation;
pub use recovery::{
    AddImagesResult, AttachmentRecoveryStatus, CreateRecoveryContract, CreateResult, FieldRecovery,
    ImageRecovery, ImageRecoveryOperation, ListingCopyReport, ObservationStatus,
    ProcessingRecoveryStatus, PublishResult, Recovery, RecoveryObservation, RecoveryStatus,
    UpdateResult, UploadRecoveryStatus, WorkflowConfig, WorkflowError, WorkflowWarning,
};
pub use types::{
    CategoryPrediction, CategoryValidation, ComposerModelStatus, Confirmation, DeliveryComposer,
    DeliveryOption, DraftDelivery, DraftImage, DraftModel, DraftState, FieldOption, ImageState,
    ListingDraftSeed, ProductContext, Publication, PublicationCategory, PublicationDraftState,
    PublicationRequirement, PublicationValidation, SourceImage, UploadedImage,
    ValidationEvidenceFailure,
};
pub use validation::{evaluate_publication, ordered_image_states};
pub use workflow::DraftWorkflow;
