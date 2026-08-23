mod cli;
mod diagnostics;
pub mod domain;
mod error;
mod image_processing;
mod invocation;
mod marketplace;
mod oauth;
mod output;
mod retry;
mod sensitive;
mod storage;
mod transport;

pub use domain::metadata::{
    AuthRequirement, CapabilityDescriptor, CapabilityId, CapabilityMaturity, MarketplaceContext,
    MarketplaceDescriptor, MarketplaceId, PortalId,
};
pub use error::{AppError, ExitClass};
pub use marketplace::{marketplace, marketplaces};

/// Dependency injection types for deterministic command tests and embedders.
pub mod dependencies {
    pub use crate::{
        cli::{
            auth::{ToriAuthArgs, ToriAuthCommand, VintedAuthArgs, VintedAuthCommand},
            outcome::{CommandData, CommandOutcome},
            runtime::ApplicationDependencies,
        },
        marketplace::{
            tori::client::{HttpError, HttpResponse, RequestSpec, ToriClient},
            vinted::{
                auth::VintedCredentialRecord,
                item::{
                    VintedItemApi, VintedItemRequest, VintedItemResult, VintedItemSession,
                    VintedItems,
                },
                search::{
                    CatalogueSearchRequest, SearchResult as VintedSearchResult, VintedSearchApi,
                },
            },
        },
        transport::{
            MultipartPart, RequestBody, Transport, TransportError, TransportErrorKind,
            TransportErrorPhase, TransportFuture, TransportRequest, TransportResponse,
        },
    };
}

#[cfg(not(test))]
use std::sync::Once;
use std::{
    ffi::OsString,
    panic::{AssertUnwindSafe, catch_unwind},
};

use clap::{CommandFactory, Parser};
use diagnostics::{DiagnosticsContext, DiagnosticsSession};
use domain::envelope::Envelope;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presentation {
    Structured,
    PlainStdout,
    PlainStderr,
}

pub struct RunResult {
    pub document: String,
    pub exit_code: u8,
    pub presentation: Presentation,
}

pub fn run<I, T>(args: I) -> RunResult
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let cli = match parse_cli(args) {
        Ok(cli) => cli,
        Err(result) => return result,
    };
    let command = cli.command.telemetry_name();
    let context = cli.command.context();

    let session = match DiagnosticsSession::initialize() {
        Ok(session) => session,
        Err(error) => {
            return finish(cli.format, Err(error.into_app_error()), None, context);
        }
    };
    let dependencies = cli::runtime::ApplicationDependencies::production();
    session.run(&command, || {
        let result = run_command(cli, &dependencies, Some(session.context()));
        let exit_code = result.exit_code;
        (result, exit_code)
    })
}

pub fn run_with_dependencies<I, T>(
    args: I,
    dependencies: &cli::runtime::ApplicationDependencies,
) -> RunResult
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let cli = match parse_cli(args) {
        Ok(cli) => cli,
        Err(result) => return result,
    };
    run_command(cli, dependencies, None)
}

fn parse_cli<I, T>(args: I) -> Result<cli::Cli, RunResult>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    #[cfg(not(test))]
    install_safe_panic_hook();
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    cli::Cli::try_parse_from(args).map_err(clap_presentation)
}

fn run_command(
    cli: cli::Cli,
    dependencies: &cli::runtime::ApplicationDependencies,
    diagnostics: Option<&DiagnosticsContext>,
) -> RunResult {
    let format = cli.format;
    let context = cli.command.context();
    catch_unwind(AssertUnwindSafe(|| {
        finish(
            format,
            execute_command(cli.command, dependencies),
            diagnostics,
            context,
        )
    }))
    .unwrap_or_else(|_| {
        finish(
            format,
            Err(AppError::unexpected("command failed unexpectedly")),
            diagnostics,
            context,
        )
    })
}

fn execute_command(
    command: cli::Command,
    dependencies: &cli::runtime::ApplicationDependencies,
) -> Result<cli::outcome::CommandOutcome, AppError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the Tokio runtime uses static configuration")
        .block_on(cli::dispatch(command, dependencies))
}

fn clap_presentation(error: clap::Error) -> RunResult {
    let presentation = if error.use_stderr() {
        Presentation::PlainStderr
    } else {
        Presentation::PlainStdout
    };
    RunResult {
        document: error.to_string(),
        exit_code: u8::try_from(error.exit_code()).unwrap_or(1),
        presentation,
    }
}

fn finish(
    format: output::OutputFormat,
    result: Result<cli::outcome::CommandOutcome, AppError>,
    diagnostics: Option<&DiagnosticsContext>,
    context: Option<marketplace::MarketplaceContext>,
) -> RunResult {
    let (envelope, exit_code) = match result {
        Ok(outcome) => {
            match output::render_plain(&outcome.presentation, format) {
                Ok(Some(document)) => {
                    return RunResult {
                        document,
                        exit_code: ExitClass::Success.code(),
                        presentation: Presentation::PlainStdout,
                    };
                }
                Ok(None) => {}
                Err(error) => return finish(format, Err(error), diagnostics, context),
            }
            let mut envelope = Envelope::success(outcome.data);
            envelope.context = context;
            envelope.next_actions = outcome.next_actions;
            envelope.observation = outcome.observation;
            envelope.warnings = outcome.warnings;
            (envelope, ExitClass::Success.code())
        }
        Err(mut error) => {
            if error.diagnostics.is_none() {
                error.diagnostics = diagnostics.map(|context| Box::new(context.envelope()));
            }
            let exit_code = error.exit_class.code();
            let diagnostic_chain = error
                .internal_chain()
                .into_iter()
                .map(|message| diagnostics::redact_diagnostic_text(&message))
                .collect::<Vec<_>>();
            let mut diagnostic_partial = error.partial.as_deref().cloned();
            if let Some(partial) = &mut diagnostic_partial {
                diagnostics::redact_diagnostic_value(partial);
            }
            tracing::error!(
                event = "command.failed",
                error.code = error.code,
                error.upstream_transient = error.upstream_transient,
                error.safe_to_retry = error.safe_to_retry,
                error.chain = ?diagnostic_chain,
                partial = ?diagnostic_partial
            );
            let mut envelope = Envelope::<cli::outcome::CommandData>::failure(error);
            envelope.context = context;
            (envelope, exit_code)
        }
    };

    match output::render(&envelope, format) {
        Ok(document) => RunResult {
            document,
            exit_code,
            presentation: Presentation::Structured,
        },
        Err(mut render_error) => {
            render_error.diagnostics = diagnostics.map(|context| Box::new(context.envelope()));
            let diagnostic_chain = render_error
                .internal_chain()
                .into_iter()
                .map(|message| diagnostics::redact_diagnostic_text(&message))
                .collect::<Vec<_>>();
            tracing::error!(
                event = "output.failed",
                error.chain = ?diagnostic_chain
            );
            let mut fallback = Envelope::<cli::outcome::CommandData>::failure(render_error);
            fallback.context = context;
            let document = serde_json::to_string(&fallback).unwrap_or_else(|_| {
                "{\"ok\":false,\"error\":{\"code\":\"output.failed\",\"message\":\"failed to serialize output\",\"upstream_transient\":false,\"safe_to_retry\":false},\"warnings\":[],\"next_actions\":[]}".to_owned()
            });
            RunResult {
                document,
                exit_code: ExitClass::Upstream.code(),
                presentation: Presentation::Structured,
            }
        }
    }
}

#[cfg(not(test))]
fn install_safe_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        std::panic::set_hook(Box::new(|_| {
            eprintln!("flea: command failed unexpectedly");
        }));
    });
}

pub fn command() -> clap::Command {
    cli::Cli::command()
}

#[cfg(test)]
extern crate self as flea;

#[cfg(test)]
#[path = "../tests/auth_login_presentation.rs"]
mod auth_login_presentation;
#[cfg(test)]
#[path = "../tests/cli_parsing.rs"]
mod cli_parsing;
#[cfg(test)]
#[path = "../tests/cli_to_envelope.rs"]
mod cli_to_envelope;
#[cfg(test)]
#[path = "../tests/command_result_characterization.rs"]
mod command_result_characterization;
#[cfg(test)]
#[path = "../tests/conformance_fixtures.rs"]
mod conformance_fixtures;
#[cfg(test)]
#[path = "../tests/draft_http_fixtures.rs"]
mod draft_http_fixtures;
#[cfg(test)]
#[path = "../tests/http_transport.rs"]
mod http_transport;
#[cfg(test)]
#[path = "../tests/item_fixtures.rs"]
mod item_fixtures;
#[cfg(test)]
#[path = "../tests/listings_fixtures.rs"]
mod listings_fixtures;
#[cfg(test)]
#[path = "../tests/output_formats.rs"]
mod output_formats;
#[cfg(test)]
#[path = "../tests/saved_search_fixtures.rs"]
mod saved_search_fixtures;
#[cfg(test)]
#[path = "../tests/search_fixtures.rs"]
mod search_fixtures;
#[cfg(test)]
#[path = "../tests/tori_error_redaction_contracts.rs"]
mod tori_error_redaction_contracts;
