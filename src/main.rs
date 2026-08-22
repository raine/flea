use std::process::ExitCode;

fn main() -> ExitCode {
    let result = tori::run(std::env::args_os());
    println!("{}", result.document);
    ExitCode::from(result.exit_code)
}
