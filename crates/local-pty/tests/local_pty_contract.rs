use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use local_pty::{LocalProcessSpec, LocalProcessSpecError, LocalPtyEndpoint, LocalPtyState};
use terminal_core::{Endpoint, EndpointError, EndpointEvent, EndpointId, EndpointSize};

fn size() -> EndpointSize {
    EndpointSize::new(80, 24).expect("valid test size")
}

fn delayed_marker_path(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "extensible-terminal-{label}-{}-{nonce}.marker",
        std::process::id()
    ))
}

fn wait_for_marker(marker: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && !marker.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "PTY child never signaled readiness at {}",
        marker.display()
    );
}

fn assert_delayed_marker_was_not_written(marker: &Path, wait: Duration) {
    std::thread::sleep(wait);
    let existed = marker.exists();
    let contents = std::fs::read_to_string(marker).ok();
    let _ = std::fs::remove_file(marker);
    assert!(
        !existed,
        "dropped PTY child survived long enough to perform delayed work: {contents:?}"
    );
}

#[cfg(unix)]
fn assert_tracked_process_was_reaped(process_id: u32) {
    let process_id = process_id.to_string();
    let output = Command::new("ps")
        .args(["-p", process_id.as_str(), "-o", "stat="])
        .output()
        .expect("inspect dropped PTY child with ps");
    assert!(
        !output.status.success(),
        "dropped PTY child still has a process-table entry: {}",
        String::from_utf8_lossy(&output.stdout).trim()
    );
}

#[cfg(windows)]
fn assert_tracked_process_was_reaped(process_id: u32) {
    let process_filter = format!("PID eq {process_id}");
    let output = Command::new("tasklist")
        .args(["/FI", process_filter.as_str(), "/NH", "/FO", "CSV"])
        .output()
        .expect("inspect dropped ConPTY child with tasklist");
    assert!(output.status.success(), "tasklist failed to inspect dropped child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let quoted_process_id = format!("\"{process_id}\"");
    assert!(
        !stdout.contains(&quoted_process_id),
        "dropped ConPTY child is still listed by tasklist: {}",
        stdout.trim()
    );
}

#[test]
fn process_spec_rejects_empty_program() {
    assert_eq!(
        LocalProcessSpec::new("", size()),
        Err(LocalProcessSpecError::EmptyProgram)
    );
}

#[test]
fn local_endpoint_does_not_claim_persistence_or_detach() {
    let spec = LocalProcessSpec::new("unused", size()).unwrap();
    let mut endpoint = LocalPtyEndpoint::new(EndpointId::new("local:test").unwrap(), spec);

    let capabilities = endpoint.capabilities();
    assert!(!capabilities.detachable);
    assert!(!capabilities.persistent);
    assert!(!capabilities.remote);
    assert!(capabilities.resize);
    assert_eq!(endpoint.state(), LocalPtyState::Created);
    assert_eq!(
        endpoint.detach(),
        Err(EndpointError::Unsupported("detach for local PTY endpoints"))
    );
}

#[cfg(unix)]
#[test]
fn native_unix_pty_emits_output_and_exit_without_shell_annotations() {
    let spec = LocalProcessSpec::new("/bin/sh", size())
        .unwrap()
        .arg("-c")
        .arg("printf COLORLESS_CORE; exit 7");
    let mut endpoint = LocalPtyEndpoint::new(
        EndpointId::new("local:unix-contract").unwrap(),
        spec,
    );
    endpoint.attach().expect("attach native PTY");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut output = Vec::new();
    let mut exit = None;

    while Instant::now() < deadline
        && (exit.is_none() || !String::from_utf8_lossy(&output).contains("COLORLESS_CORE"))
    {
        match endpoint.poll_event().expect("poll local PTY") {
            Some(EndpointEvent::Output(bytes)) => output.extend(bytes),
            Some(EndpointEvent::Exited { code, .. }) => exit = code,
            Some(_) | None => std::thread::sleep(Duration::from_millis(10)),
        }
    }

    assert!(String::from_utf8_lossy(&output).contains("COLORLESS_CORE"));
    assert_eq!(exit, Some(7));
    assert_eq!(endpoint.state(), LocalPtyState::Exited);
}

#[cfg(unix)]
#[test]
fn unix_final_output_is_delivered_before_exit() {
    let spec = LocalProcessSpec::new("/bin/sh", size())
        .unwrap()
        .arg("-c")
        .arg("printf FINAL_OUTPUT_BEFORE_EXIT; exit 0");
    let mut endpoint = LocalPtyEndpoint::new(
        EndpointId::new("local:unix-final-output").unwrap(),
        spec,
    );
    endpoint.attach().expect("attach native PTY");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut output = Vec::new();
    let mut saw_exit = false;
    while Instant::now() < deadline && !saw_exit {
        match endpoint.poll_event().expect("poll local PTY") {
            Some(EndpointEvent::Output(bytes)) => output.extend(bytes),
            Some(EndpointEvent::Exited { .. }) => saw_exit = true,
            Some(_) | None => std::thread::sleep(Duration::from_millis(1)),
        }
    }

    assert!(saw_exit, "fast child never reported exit");
    assert!(
        String::from_utf8_lossy(&output).contains("FINAL_OUTPUT_BEFORE_EXIT"),
        "exit was surfaced before final PTY output: {}",
        String::from_utf8_lossy(&output).escape_debug()
    );
}

#[cfg(unix)]
#[test]
fn dropping_running_unix_endpoint_terminates_and_reaps_tracked_child() {
    let ready_marker = delayed_marker_path("drop-unix-ready");
    let survived_marker = delayed_marker_path("drop-unix-survived");
    let _ = std::fs::remove_file(&ready_marker);
    let _ = std::fs::remove_file(&survived_marker);
    let spec = LocalProcessSpec::new("/bin/sh", size())
        .unwrap()
        .arg("-c")
        .arg(
            "printf READY > \"$TERMINAL_DROP_READY\"; sleep 1; printf SURVIVED > \"$TERMINAL_DROP_SURVIVED\"",
        )
        .with_env("TERMINAL_DROP_READY", ready_marker.clone().into_os_string())
        .with_env(
            "TERMINAL_DROP_SURVIVED",
            survived_marker.clone().into_os_string(),
        );
    let mut endpoint = LocalPtyEndpoint::new(
        EndpointId::new("local:unix-drop-contract").unwrap(),
        spec,
    );
    endpoint.attach().expect("attach native PTY");
    assert_eq!(endpoint.state(), LocalPtyState::Running);
    let process_id = endpoint.process_id().expect("running PTY child has a PID");

    wait_for_marker(&ready_marker, Duration::from_secs(3));
    drop(endpoint);

    assert_tracked_process_was_reaped(process_id);
    assert_delayed_marker_was_not_written(&survived_marker, Duration::from_millis(1_200));
    let _ = std::fs::remove_file(ready_marker);
}

#[cfg(windows)]
fn consume_conpty_output(
    endpoint: &mut LocalPtyEndpoint,
    output: &mut Vec<u8>,
    exit: &mut Option<i32>,
) {
    match endpoint.poll_event().expect("poll ConPTY") {
        Some(EndpointEvent::Output(bytes)) => {
            let cursor_query = bytes.windows(4).any(|window| window == b"\x1b[6n");
            output.extend(bytes);
            if cursor_query {
                endpoint
                    .send(b"\x1b[1;1R")
                    .expect("answer ConPTY cursor position query");
            }
        }
        Some(EndpointEvent::Exited { code, .. }) => *exit = code,
        Some(_) | None => std::thread::sleep(Duration::from_millis(10)),
    }
}

#[cfg(windows)]
#[test]
fn native_windows_conpty_supports_bidirectional_io_and_exit() {
    // ConPTY may ask its terminal peer for cursor state before cmd.exe becomes
    // interactive. The test answers that VT query so it exercises the provider
    // as a real terminal would, without moving VT semantics into local-pty.
    let spec = LocalProcessSpec::new("cmd.exe", size())
        .unwrap()
        .arg("/D")
        .arg("/Q")
        .arg("/K")
        .arg("echo CONPTY_READY");
    let mut endpoint = LocalPtyEndpoint::new(
        EndpointId::new("local:conpty-contract").unwrap(),
        spec,
    );
    endpoint.attach().expect("attach native ConPTY");

    let ready_deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    let mut exit = None;
    while Instant::now() < ready_deadline
        && !String::from_utf8_lossy(&output).contains("CONPTY_READY")
    {
        consume_conpty_output(&mut endpoint, &mut output, &mut exit);
    }
    assert!(
        String::from_utf8_lossy(&output).contains("CONPTY_READY"),
        "ConPTY child never produced readiness output: {}",
        String::from_utf8_lossy(&output).escape_debug()
    );

    endpoint
        .send(b"echo COLORLESS_CORE\r\nexit /b 7\r\n")
        .expect("send commands through ConPTY");

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline
        && (exit.is_none() || !String::from_utf8_lossy(&output).contains("COLORLESS_CORE"))
    {
        consume_conpty_output(&mut endpoint, &mut output, &mut exit);
    }

    assert!(
        String::from_utf8_lossy(&output).contains("COLORLESS_CORE"),
        "ConPTY did not echo input command output: {}",
        String::from_utf8_lossy(&output).escape_debug()
    );
    assert_eq!(exit, Some(7));
    assert_eq!(endpoint.state(), LocalPtyState::Exited);
}

#[cfg(windows)]
#[test]
fn windows_final_output_is_delivered_before_exit() {
    let spec = LocalProcessSpec::new("cmd.exe", size())
        .unwrap()
        .arg("/D")
        .arg("/Q")
        .arg("/C")
        .arg("echo FINAL_OUTPUT_BEFORE_EXIT");
    let mut endpoint = LocalPtyEndpoint::new(
        EndpointId::new("local:windows-final-output").unwrap(),
        spec,
    );
    endpoint.attach().expect("attach native ConPTY");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    let mut exit = None;
    while Instant::now() < deadline && exit.is_none() {
        consume_conpty_output(&mut endpoint, &mut output, &mut exit);
    }

    assert_eq!(exit, Some(0));
    assert!(
        String::from_utf8_lossy(&output).contains("FINAL_OUTPUT_BEFORE_EXIT"),
        "exit was surfaced before final ConPTY output: {}",
        String::from_utf8_lossy(&output).escape_debug()
    );
}

#[cfg(windows)]
#[test]
fn dropping_running_windows_endpoint_terminates_and_reaps_tracked_child() {
    let ready_marker = delayed_marker_path("drop-windows-ready");
    let survived_marker = delayed_marker_path("drop-windows-survived");
    let _ = std::fs::remove_file(&ready_marker);
    let _ = std::fs::remove_file(&survived_marker);
    let spec = LocalProcessSpec::new("cmd.exe", size())
        .unwrap()
        .arg("/D")
        .arg("/Q")
        .arg("/C")
        .arg(
            "echo READY>\"%TERMINAL_DROP_READY%\" & ping -n 3 127.0.0.1 >nul & echo SURVIVED>\"%TERMINAL_DROP_SURVIVED%\"",
        )
        .with_env("TERMINAL_DROP_READY", ready_marker.clone().into_os_string())
        .with_env(
            "TERMINAL_DROP_SURVIVED",
            survived_marker.clone().into_os_string(),
        );
    let mut endpoint = LocalPtyEndpoint::new(
        EndpointId::new("local:windows-drop-contract").unwrap(),
        spec,
    );
    endpoint.attach().expect("attach native ConPTY");
    assert_eq!(endpoint.state(), LocalPtyState::Running);
    let process_id = endpoint.process_id().expect("running ConPTY child has a PID");

    wait_for_marker(&ready_marker, Duration::from_secs(5));
    drop(endpoint);

    assert_tracked_process_was_reaped(process_id);
    assert_delayed_marker_was_not_written(&survived_marker, Duration::from_secs(3));
    let _ = std::fs::remove_file(ready_marker);
}
