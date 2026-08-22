use std::process::ExitCode;

use flea::Presentation;

fn main() -> ExitCode {
    let result = flea::run(std::env::args_os());
    match result.presentation {
        Presentation::Structured => println!("{}", result.document),
        Presentation::PlainStdout => print!("{}", result.document),
        Presentation::PlainStderr => eprint!("{}", result.document),
    }
    ExitCode::from(result.exit_code)
}
