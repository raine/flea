#![cfg(unix)]

use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

fn command(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_flea"));
    command
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"));
    command
}

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn installed_bridge_serves_real_cli_auth_without_debugging() {
    // Keep the native socket path below macOS's Unix socket path limit.
    let home = tempfile::Builder::new()
        .prefix("flea-")
        .tempdir_in("/tmp")
        .unwrap();
    let setup = command(home.path())
        .args(["extension", "setup", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stdout)
    );
    let setup: Value = serde_json::from_slice(&setup.stdout).unwrap();
    let data = &setup["data"];
    let extension = std::path::Path::new(data["extension_path"].as_str().unwrap());
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(data["native_host_manifest"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let origin = manifest["allowed_origins"][0].as_str().unwrap();
    let mut host = Host(
        command(home.path())
            .arg(origin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = extension.parent().unwrap().join("bridge.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "native host did not start");
        assert!(host.0.try_wait().unwrap().is_none(), "native host exited");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut input = host.0.stdout.take().unwrap();
    let mut output = host.0.stdin.take().unwrap();
    let responder = std::thread::spawn(move || {
        let mut text = String::new();
        loop {
            let mut length = [0; 4];
            input.read_exact(&mut length).unwrap();
            let length = u32::from_le_bytes(length) as usize;
            assert!(length < 1024 * 1024);
            let mut bytes = vec![0; length];
            input.read_exact(&mut bytes).unwrap();
            let chunk: Value = serde_json::from_slice(&bytes).unwrap();
            text.push_str(chunk["data"].as_str().unwrap());
            if chunk["index"].as_u64().unwrap() + 1 == chunk["total"].as_u64().unwrap() {
                break;
            }
        }
        let request: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            request["command"],
            json!({"action": "request", "method": "GET", "path": "/api/v2/users/current"})
        );
        let response =
            json!({"id":request["id"], "result":{"status":200,"body":{"user":{"id":42}}}});
        let chunk = serde_json::to_vec(&json!({"type":"chunk","id":request["id"],"index":0,"total":1,"data":response.to_string()})).unwrap();
        output
            .write_all(&(chunk.len() as u32).to_le_bytes())
            .unwrap();
        output.write_all(&chunk).unwrap();
        output.flush().unwrap();
        // Keep stdin alive until the host has delivered the response to the CLI.
        output
    });
    let mut cli = command(home.path())
        .args(["vinted", "auth", "status", "--browser", "--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if wait_timeout::ChildExt::wait_timeout(&mut cli, Duration::from_secs(10))
        .unwrap()
        .is_none()
    {
        cli.kill().unwrap();
        panic!("extension auth command timed out");
    }
    let result = cli.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let result: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["data"]["authenticated"], true);
    let native_stdin = responder.join().unwrap();

    let logout = command(home.path())
        .args(["vinted", "auth", "logout", "--browser", "--format", "json"])
        .output()
        .unwrap();
    assert!(!logout.status.success());
    assert!(String::from_utf8_lossy(&logout.stdout).contains("sign out on the Vinted website"));
    drop(native_stdin);
}

#[test]
fn rejects_other_extensions_without_emitting_cli_json_on_native_stdout() {
    let home = tempfile::tempdir().unwrap();
    let result = command(home.path())
        .arg("chrome-extension://untrusted/")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}

#[test]
fn setup_defaults_to_instructions_and_preserves_explicit_formats() {
    let home = tempfile::Builder::new()
        .prefix("flea-")
        .tempdir_in("/tmp")
        .unwrap();
    let plain = command(home.path())
        .args(["extension", "setup"])
        .output()
        .unwrap();
    assert!(plain.status.success());
    let plain = String::from_utf8(plain.stdout).unwrap();
    assert!(plain.starts_with("Flea's Chrome bridge is installed.\n"));
    assert!(plain.contains("1. Open chrome://extensions"));
    assert!(plain.contains("flea vinted auth status --browser"));
    assert!(!plain.contains("extension_id:"));
    assert!(!plain.contains("copied to clipboard"));
    if cfg!(target_os = "macos") {
        assert!(plain.contains("Command+Shift+G"));
    }
    for format in ["json", "toon"] {
        let output = command(home.path())
            .args(["extension", "setup", "--format", format])
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(!output.starts_with("Flea's Chrome bridge"));
        assert!(output.contains("extension_id"));
        if format == "json" {
            let value: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(value["ok"], true);
            assert!(value["data"]["next_steps"].is_array());
        } else {
            assert!(output.starts_with("ok: true\n"));
        }
    }
}
