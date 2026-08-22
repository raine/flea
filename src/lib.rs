pub mod api;
pub mod cli;
pub mod diagnostics;
pub mod domain;
pub mod error;
pub mod output;
pub mod storage;

use std::{
    any::Any,
    ffi::OsString,
    panic::{AssertUnwindSafe, catch_unwind},
};

use clap::{CommandFactory, Parser, error::ErrorKind};
use diagnostics::{DiagnosticsContext, DiagnosticsSession};
use domain::envelope::{Envelope, Warning};
use error::{AppError, ExitClass};

pub struct RunResult {
    pub document: String,
    pub exit_code: u8,
}

pub fn run<I, T>(args: I) -> RunResult
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let requested_format = output::format_from_args(args.iter().cloned()).unwrap_or_default();
    let command = diagnostics::command_name(&args);

    let session = match DiagnosticsSession::initialize() {
        Ok(session) => session,
        Err(error) => return finish(requested_format, Err(error.into_app_error()), None),
    };
    let runtime = cli::runtime::ProductionRuntime;
    session.run(&command, || {
        let result = catch_unwind(AssertUnwindSafe(|| {
            run_parsed(args, requested_format, Some(session.context()), &runtime)
        }))
        .unwrap_or_else(|panic| {
            finish(
                requested_format,
                Err(AppError::unexpected(panic_message(panic))),
                Some(session.context()),
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
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let requested_format = output::format_from_args(args.iter().cloned()).unwrap_or_default();
    catch_unwind(AssertUnwindSafe(|| {
        run_parsed(args, requested_format, None, runtime)
    }))
    .unwrap_or_else(|panic| {
        finish(
            requested_format,
            Err(AppError::unexpected(panic_message(panic))),
            None,
        )
    })
}

fn run_parsed(
    args: Vec<OsString>,
    requested_format: output::OutputFormat,
    diagnostics: Option<&DiagnosticsContext>,
    runtime: &dyn cli::CommandRuntime,
) -> RunResult {
    match cli::Cli::try_parse_from(args) {
        Ok(cli) => finish(
            cli.format,
            cli::dispatch_with_runtime(cli.command, runtime),
            diagnostics,
        ),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            finish(
                requested_format,
                Ok(serde_json::json!({ "text": error.to_string() })),
                diagnostics,
            )
        }
        Err(error) => finish(
            requested_format,
            Err(AppError::usage(error.to_string())),
            diagnostics,
        ),
    }
}

fn finish(
    format: output::OutputFormat,
    result: Result<serde_json::Value, AppError>,
    diagnostics: Option<&DiagnosticsContext>,
) -> RunResult {
    let (envelope, exit_code) = match result {
        Ok(data) => {
            let mut envelope = Envelope::success(data);
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
            }
        }
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "command panicked".to_owned()
}

pub fn command() -> clap::Command {
    cli::Cli::command()
}
