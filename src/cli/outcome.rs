use serde::Serialize;
use serde_json::Value;

use crate::marketplace::tori::adinput::{
    AddImagesResult, CreateResult, DraftState, PublicationValidation, PublishResult, UpdateResult,
};
use crate::{
    cli::{draft::DraftInspectionOutput, draft_input::DraftPreviewOutput, skill::SkillOutput},
    domain::{
        envelope::{NextAction, Warning},
        item::PublicItemDetail,
        listing::{
            CategoryList, CategorySearchResult, ListingCollection, ListingDetail, ListingMutation,
        },
        observation::Observation,
        search::{LocationCollection, SearchCollection},
    },
    marketplace::{
        CapabilityDescriptor, MarketplaceDescriptor, MarketplaceId, PortalId,
        tori::{
            favorites::{FavoriteFolder, FavoriteMutation, FavoriteStatus},
            interactive::CallbackCapture,
            login::{AuthCompleteOutput, AuthLogoutOutput},
            saved_searches::{DeletedSavedSearch, SavedSearch},
            session::AuthStatusOutput,
        },
        vinted::{
            auth::VintedLoginResult,
            session::{VintedAuthStatus, VintedLogoutOutput},
        },
    },
};

#[derive(Debug, Serialize)]
pub struct CapabilitiesOutput {
    pub marketplaces: &'static [MarketplaceDescriptor],
}

#[derive(Debug, Serialize)]
pub struct MarketplaceSummary {
    pub marketplace: MarketplaceId,
    pub portals: &'static [PortalId],
}

#[derive(Debug, Serialize)]
pub struct MarketplacesOutput {
    pub marketplaces: Vec<MarketplaceSummary>,
}

#[derive(Debug, Serialize)]
pub struct MarketplaceCapabilitiesOutput {
    pub marketplace: MarketplaceId,
    pub portals: &'static [PortalId],
    pub capabilities: &'static [CapabilityDescriptor],
}

#[derive(Debug, Serialize)]
pub struct FavoriteFoldersOutput {
    pub folders: Vec<FavoriteFolder>,
}

#[derive(Debug, Serialize)]
pub struct SavedSearchListOutput {
    pub saved_searches: Vec<SavedSearch>,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct ListingDeleteOutput {
    pub listing_id: String,
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
pub struct DraftDeleteOutput {
    pub draft_id: String,
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CommandData {
    Capabilities(CapabilitiesOutput),
    Marketplaces(MarketplacesOutput),
    MarketplaceCapabilities(MarketplaceCapabilitiesOutput),
    Skill(SkillOutput),
    ToriAuthComplete(AuthCompleteOutput),
    ToriAuthCallback(CallbackCapture),
    ToriAuthLogout(AuthLogoutOutput),
    ToriAuthStatus(AuthStatusOutput),
    VintedAuthLogin(VintedLoginResult),
    VintedAuthLogout(VintedLogoutOutput),
    VintedAuthStatus(VintedAuthStatus),
    CategorySearch(CategorySearchResult),
    CategoryList(CategoryList),
    FavoriteFolders(FavoriteFoldersOutput),
    FavoriteStatus(FavoriteStatus),
    FavoriteMutation(FavoriteMutation),
    Item(PublicItemDetail),
    Search(SearchCollection),
    Location(LocationCollection),
    SavedSearchList(SavedSearchListOutput),
    SavedSearch(SavedSearch),
    DeletedSavedSearch(DeletedSavedSearch),
    ListingCollection(ListingCollection),
    ListingDetail(ListingDetail),
    ListingMutation(ListingMutation),
    ListingDelete(ListingDeleteOutput),
    DraftPreview(DraftPreviewOutput),
    DraftInspection(DraftInspectionOutput),
    DraftCreate(CreateResult),
    DraftUpdate(UpdateResult),
    DraftAddImages(AddImagesResult),
    DraftState(DraftState),
    DraftValidation(PublicationValidation),
    DraftPublish(PublishResult),
    DraftDelete(DraftDeleteOutput),
    Raw(Value),
}

#[derive(Debug)]
pub struct CommandOutcome {
    pub data: CommandData,
    pub next_actions: Vec<NextAction>,
    pub observation: Option<Observation>,
    pub warnings: Vec<Warning>,
}

impl CommandOutcome {
    pub fn new(data: CommandData) -> Self {
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
