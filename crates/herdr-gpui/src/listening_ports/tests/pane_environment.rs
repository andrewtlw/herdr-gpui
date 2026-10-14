use super::*;
use crate::listening_ports::{STEP_TIMEOUT, local_shell, scan::COMMAND};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};

/// A TCP listener started as if from a Herdr pane, killed and reaped when
/// dropped so a failed assertion never leaves it running.
struct PaneListener {
    child: Child,
    port: u16,
}

impl PaneListener {
    /// `None` where this machine has no `python3` to listen with.
    fn start(workspace: &str, socket: &str) -> Option<Self> {
        let mut child = Command::new("python3")
            .args([
                "-c",
                "import socket, sys, time\n\
                 s = socket.socket()\n\
                 s.bind(('127.0.0.1', 0))\n\
                 s.listen()\n\
                 print(s.getsockname()[1], flush=True)\n\
                 time.sleep(60)",
            ])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HERDR_WORKSPACE_ID", workspace)
            .env("HERDR_SOCKET_PATH", socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut line = String::new();
        let stdout = child.stdout.take()?;
        BufReader::new(stdout).read_line(&mut line).ok()?;
        let Ok(port) = line.trim().parse() else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        Some(Self { child, port })
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for PaneListener {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn environment_line(listener: &PaneListener, workspace: &str, socket: &str) -> String {
    format!("E {} {workspace} {socket}", listener.pid())
}

/// The real step, run repeatedly in one long-lived shell as the worker runs
/// it: a pane's listener is named on the first scan, still named once the
/// shell answers from what it remembers, joined by one started later, and
/// forgotten once it stops listening.
#[test]
fn pane_listeners_stay_named_across_scans_and_are_forgotten_when_they_stop() {
    let (workspace, socket) = ("wScan1", "/tmp/herdr-scan-test/herdr.sock");
    let Some(first) = PaneListener::start(workspace, socket) else {
        eprintln!("skipped: python3 is not available to start a listener");
        return;
    };
    let mut shell = local_shell().unwrap();
    let mut scan = || shell.run(COMMAND, STEP_TIMEOUT).unwrap().stdout;
    let output = scan();
    if output.lines().any(|line| line == "N") {
        eprintln!("skipped: neither ss nor lsof is installed");
        return;
    }
    let first_line = environment_line(&first, workspace, socket);
    assert!(
        output.lines().any(|line| line == first_line),
        "{first_line:?} missing from {output:?}"
    );
    let output = scan();
    assert!(
        output.lines().any(|line| line == first_line),
        "a remembered listener must still be named: {output:?}"
    );
    let ports = parse(&output).unwrap();
    assert!(
        of(&ports, workspace)
            .iter()
            .any(|port| port.number == first.port)
    );

    let (later_workspace, later_socket) = ("wScan2", "/tmp/herdr-scan-test/other.sock");
    let later = PaneListener::start(later_workspace, later_socket).unwrap();
    let output = scan();
    let later_line = environment_line(&later, later_workspace, later_socket);
    assert!(
        output.lines().any(|line| line == first_line)
            && output.lines().any(|line| line == later_line),
        "a new listener is read beside a remembered one: {output:?}"
    );

    let first_pid = first.pid();
    drop(first);
    let output = scan();
    assert!(
        !output
            .lines()
            .any(|line| line.starts_with(&format!("E {first_pid} "))),
        "a stopped listener is forgotten: {output:?}"
    );
    assert!(output.lines().any(|line| line == later_line));
}
