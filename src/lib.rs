pub mod cli;
pub mod diagnostics;
pub mod domain;
pub mod error;
mod image_processing;
pub mod invocation;
pub mod marketplace;
pub(crate) mod oauth;
pub mod output;
pub mod retry;
pub(crate) mod sensitive;
pub mod storage;

use std::{
    ffi::OsString,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Once,
};

use clap::{CommandFactory, Parser};
use diagnostics::{DiagnosticsContext, DiagnosticsSession};
use domain::envelope::Envelope;
use error::{AppError, ExitClass};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlainPresentation {
    Structured,
    AuthLogin,
    Skill,
}

pub fn run<I, T>(args: I) -> RunResult
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    install_safe_panic_hook();
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match cli::Cli::try_parse_from(args.iter().cloned()) {
        Ok(cli) => cli,
        Err(error) => return clap_presentation(error),
    };
    let command = cli.command.telemetry_name();
    let context = cli.command.context();

    let session = match DiagnosticsSession::initialize() {
        Ok(session) => session,
        Err(error) => {
            return finish(
                cli.format,
                Err(error.into_app_error()),
                None,
                PlainPresentation::Structured,
                context,
            );
        }
    };
    let plain_presentation = plain_presentation(cli.format, &cli.command);
    let dependencies = cli::runtime::ApplicationDependencies::production();
    session.run(&command, || {
        let result = catch_unwind(AssertUnwindSafe(|| {
            finish(
                cli.format,
                execute_command(cli.command, &dependencies),
                Some(session.context()),
                plain_presentation,
                context,
            )
        }))
        .unwrap_or_else(|_| {
            finish(
                cli.format,
                Err(AppError::unexpected("command failed unexpectedly")),
                Some(session.context()),
                PlainPresentation::Structured,
                context,
            )
        });
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
    install_safe_panic_hook();
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match cli::Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => return clap_presentation(error),
    };
    let plain_presentation = plain_presentation(cli.format, &cli.command);
    let context = cli.command.context();
    catch_unwind(AssertUnwindSafe(|| {
        finish(
            cli.format,
            execute_command(cli.command, dependencies),
            None,
            plain_presentation,
            context,
        )
    }))
    .unwrap_or_else(|_| {
        finish(
            cli.format,
            Err(AppError::unexpected("command failed unexpectedly")),
            None,
            PlainPresentation::Structured,
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

fn plain_presentation(format: output::OutputFormat, command: &cli::Command) -> PlainPresentation {
    match command {
        cli::Command::Skill(_) => PlainPresentation::Skill,
        cli::Command::Tori(cli::ToriArgs {
            command:
                cli::ToriCommand::Auth(cli::auth::ToriAuthArgs {
                    command: cli::auth::ToriAuthCommand::Login,
                }),
        }) if format == output::OutputFormat::Toon => PlainPresentation::AuthLogin,
        cli::Command::Vinted(cli::VintedArgs {
            command:
                cli::VintedCommand::Auth(cli::auth::VintedAuthArgs {
                    command: cli::auth::VintedAuthCommand::Login,
                }),
            ..
        }) if format == output::OutputFormat::Toon => PlainPresentation::AuthLogin,
        _ => PlainPresentation::Structured,
    }
}

fn finish(
    format: output::OutputFormat,
    result: Result<cli::outcome::CommandOutcome, AppError>,
    diagnostics: Option<&DiagnosticsContext>,
    plain_presentation: PlainPresentation,
    context: Option<marketplace::MarketplaceContext>,
) -> RunResult {
    let (envelope, exit_code) = match result {
        Ok(outcome) => {
            let plain_document = match plain_presentation {
                PlainPresentation::Structured => None,
                PlainPresentation::AuthLogin => {
                    Some(output::render_auth_login(&outcome.data, context))
                }
                PlainPresentation::Skill => Some(output::render_skill(&outcome.data)),
            };
            if let Some(document) = plain_document {
                return match document {
                    Ok(document) => RunResult {
                        document,
                        exit_code: ExitClass::Success.code(),
                        presentation: Presentation::PlainStdout,
                    },
                    Err(error) => finish(
                        format,
                        Err(error),
                        diagnostics,
                        PlainPresentation::Structured,
                        context,
                    ),
                };
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
            let mut envelope = Envelope::failure(error);
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
            let mut fallback = Envelope::failure(render_error);
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
