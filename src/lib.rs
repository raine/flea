pub mod api;
pub mod cli;
pub mod diagnostics;
pub mod domain;
pub mod error;
pub mod output;
pub mod storage;

use std::{
    ffi::OsString,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Once,
};

use clap::{CommandFactory, Parser};
use diagnostics::{DiagnosticsContext, DiagnosticsSession};
use domain::envelope::{Envelope, Warning};
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
enum AuthPresentation {
    Structured,
    Start,
    Login,
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
    let command = diagnostics::command_name(&args);

    let session = match DiagnosticsSession::initialize() {
        Ok(session) => session,
        Err(error) => {
            return finish(
                cli.format,
                Err(error.into_app_error()),
                None,
                AuthPresentation::Structured,
            );
        }
    };
    let auth_presentation = auth_presentation(cli.format, &cli.command);
    let runtime = cli::runtime::ProductionRuntime;
    session.run(&command, || {
        let result = catch_unwind(AssertUnwindSafe(|| {
            finish(
                cli.format,
                cli::dispatch_with_runtime(cli.command, &runtime),
                Some(session.context()),
                auth_presentation,
            )
        }))
        .unwrap_or_else(|_| {
            finish(
                cli.format,
                Err(AppError::unexpected("command failed unexpectedly")),
                Some(session.context()),
                AuthPresentation::Structured,
            )
        });
        let exit_code = result.exit_code;
        (result, exit_code)
    })
}

pub fn run_with_runtime<I, T>(args: I, runtime: &dyn cli::CommandRuntime) -> RunResult
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
    let auth_presentation = auth_presentation(cli.format, &cli.command);
    catch_unwind(AssertUnwindSafe(|| {
        finish(
            cli.format,
            cli::dispatch_with_runtime(cli.command, runtime),
            None,
            auth_presentation,
        )
    }))
    .unwrap_or_else(|_| {
        finish(
            cli.format,
            Err(AppError::unexpected("command failed unexpectedly")),
            None,
            AuthPresentation::Structured,
        )
    })
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

fn auth_presentation(format: output::OutputFormat, command: &cli::Command) -> AuthPresentation {
    if format != output::OutputFormat::Toon {
        return AuthPresentation::Structured;
    }
    match command {
        cli::Command::Auth(cli::auth::AuthArgs {
            command: cli::auth::AuthCommand::Start,
        }) => AuthPresentation::Start,
        cli::Command::Auth(cli::auth::AuthArgs {
            command: cli::auth::AuthCommand::Login,
        }) => AuthPresentation::Login,
        _ => AuthPresentation::Structured,
    }
}

fn finish(
    format: output::OutputFormat,
    result: Result<serde_json::Value, AppError>,
    diagnostics: Option<&DiagnosticsContext>,
    auth_presentation: AuthPresentation,
) -> RunResult {
    let (envelope, exit_code) = match result {
        Ok(mut data) => {
            let plain_document = match auth_presentation {
                AuthPresentation::Structured => None,
                AuthPresentation::Start => Some(output::render_auth_start(&data)),
                AuthPresentation::Login => Some(output::render_auth_login(&data)),
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
                        AuthPresentation::Structured,
                    ),
                };
            }
            let next_actions = data
                .as_object_mut()
                .and_then(|object| object.remove("_next_actions"))
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            let mut envelope = Envelope::success(data);
            envelope.next_actions = next_actions;
            if let Some(warnings) = envelope
                .data
                .as_ref()
                .and_then(|data| data.get("warnings"))
                .and_then(serde_json::Value::as_array)
            {
                envelope.warnings = warnings
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|message| Warning {
                        code: "workflow.best_effort_failed".to_owned(),
                        message: message.to_owned(),
                    })
                    .collect();
            }
            (envelope, ExitClass::Success.code())
        }
        Err(mut error) => {
            if error.diagnostics.is_none() {
                error.diagnostics = diagnostics.map(|context| Box::new(context.envelope()));
            }
            let exit_code = error.exit_class.code();
            tracing::error!(
                event = "command.failed",
                error.code = error.code,
                error.retryable = error.retryable,
                error.chain = ?error.internal_chain(),
                partial = ?error.partial
            );
            (Envelope::failure(error), exit_code)
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
            tracing::error!(
                event = "output.failed",
                error.chain = ?render_error.internal_chain()
            );
            let fallback = Envelope::failure(render_error);
            let document = serde_json::to_string(&fallback).unwrap_or_else(|_| {
                "{\"ok\":false,\"error\":{\"code\":\"output.failed\",\"message\":\"failed to serialize output\",\"retryable\":false},\"warnings\":[],\"next_actions\":[]}".to_owned()
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
            eprintln!("tori: command failed unexpectedly");
        }));
    });
}

pub fn command() -> clap::Command {
    cli::Cli::command()
}
