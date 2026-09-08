use std::process::ExitCode;

use flea::Presentation;

fn main() -> ExitCode {
    if let Some(origin) = std::env::args_os()
        .nth(1)
        .and_then(|arg| arg.into_string().ok())
        .filter(|arg| arg.starts_with("chrome-extension://"))
    {
        return match flea::run_extension_host(&origin) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => {
                eprintln!(
                    "Flea native bridge stopped; reload the extension and inspect remote state before retrying"
                );
                ExitCode::FAILURE
            }
        };
    }
    let result = flea::run(std::env::args_os());
    match result.presentation {
        Presentation::Structured => println!("{}", result.document),
        Presentation::PlainStdout => print!("{}", result.document),
        Presentation::PlainStderr => eprint!("{}", result.document),
    }
    ExitCode::from(result.exit_code)
}
