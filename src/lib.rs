pub mod api;
pub mod cli;
pub mod diagnostics;
pub mod domain;
pub mod error;
pub mod output;
pub mod storage;

use std::ffi::OsString;

use clap::{CommandFactory, Parser, error::ErrorKind};
use domain::envelope::Envelope;
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

    match cli::Cli::try_parse_from(args) {
        Ok(cli) => finish(cli.format, cli::dispatch(cli.command)),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            finish(
                requested_format,
                Ok(serde_json::json!({ "text": error.to_string() })),
            )
        }
        Err(error) => finish(requested_format, Err(AppError::usage(error.to_string()))),
    }
}

fn finish(format: output::OutputFormat, result: Result<serde_json::Value, AppError>) -> RunResult {
    let (envelope, exit_code) = match result {
        Ok(data) => (Envelope::success(data), ExitClass::Success.code()),
        Err(error) => {
            let exit_code = error.exit_class.code();
            (Envelope::failure(error), exit_code)
        }
    };

    match output::render(&envelope, format) {
        Ok(document) => RunResult {
            document,
            exit_code,
        },
        Err(render_error) => {
            let fallback = Envelope::failure(render_error);
            let document = serde_json::to_string(&fallback)
                .unwrap_or_else(|_| "{\"ok\":false,\"error\":{\"code\":\"output.failed\",\"message\":\"failed to serialize output\",\"retryable\":false}}".to_owned());
            RunResult {
                document,
                exit_code: ExitClass::Upstream.code(),
            }
        }
    }
}

pub fn command() -> clap::Command {
    cli::Cli::command()
}
