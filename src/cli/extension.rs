use crate::error::AppError;

use super::outcome::{CommandData, CommandOutcome, CommandPresentation, PlainOutput};

pub fn setup(copy_to_clipboard: bool) -> Result<CommandOutcome, AppError> {
    let result = crate::extension::setup()?;
    let path = result["extension_path"]
        .as_str()
        .ok_or_else(|| AppError::output("extension setup returned no extension directory"))?;
    let copied = copy_to_clipboard && copy_path(path);
    let document = setup_document(path, cfg!(target_os = "macos"), copied);
    let mut outcome = CommandOutcome::new(CommandData::ExtensionSetup(result));
    outcome.presentation =
        CommandPresentation::Plain(PlainOutput::ExtensionSetupDocument(document));
    Ok(outcome)
}

fn setup_document(path: &str, macos: bool, copied: bool) -> String {
    let picker_tip = if macos {
        if copied {
            "   Path copied to clipboard. Press Command+Shift+G in the folder\n   picker, paste with Command+V, then press Return and click Select.\n\n"
        } else {
            "   In the folder picker, press Command+Shift+G, paste the path\n   above, then press Return and click Select.\n\n"
        }
    } else {
        ""
    };
    format!(
        "Flea's Chrome bridge is installed.\n\n\
         Finish setup in Chrome:\n\n\
         1. Open chrome://extensions and turn on Developer mode.\n\
         2. Click Load unpacked and select this folder:\n\n   \
             {path}\n\n\
         {picker_tip}\
         3. Open or reload one https://www.vinted.fi tab and sign in.\n\n\
         Then check the connection:\n\n   \
             flea vinted auth status --browser\n\n\
         No separate Chrome window or debugging port is needed.\n\
         If the extension is already loaded, click its Reload button instead.\n"
    )
}

#[cfg(target_os = "macos")]
fn copy_path(path: &str) -> bool {
    use std::{
        io::{IsTerminal, Write},
        process::{Command, Stdio},
        time::Duration,
    };
    use wait_timeout::ChildExt;

    if !std::io::stdout().is_terminal() {
        return false;
    }
    let Ok(mut child) = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let written = child
        .stdin
        .take()
        .is_some_and(|mut input| input.write_all(path.as_bytes()).is_ok());
    match child.wait_timeout(Duration::from_secs(2)) {
        Ok(Some(status)) => written && status.success(),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            false
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn copy_path(_path: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_instructions_explain_the_picker_and_confirm_only_successful_copy() {
        let document = setup_document(
            "/Users/test/Library/Application Support/flea/extension",
            true,
            true,
        );
        assert!(document.contains("Path copied to clipboard"));
        assert!(document.contains("Command+Shift+G"));
        assert!(document.contains("Command+V"));
        assert!(document.contains("\n   /Users/test/Library/Application Support/flea/extension\n"));
        assert!(document.contains("flea vinted auth status --browser"));
        let fallback = setup_document("/extension", true, false);
        assert!(!fallback.contains("copied to clipboard"));
        assert!(fallback.contains("Command+Shift+G"));
    }

    #[test]
    fn linux_instructions_do_not_claim_clipboard_or_macos_shortcuts() {
        let document = setup_document("/home/test/extension", false, false);
        assert!(document.contains("1. Open chrome://extensions"));
        assert!(document.contains("2. Click Load unpacked"));
        assert!(document.contains("3. Open or reload"));
        assert!(!document.contains("clipboard"));
        assert!(!document.contains("Command+Shift+G"));
    }
}
