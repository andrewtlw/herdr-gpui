use super::*;
use std::{io::BufRead, os::unix::process::CommandExt};

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn gone_within(pid: u32, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// Starts `/bin/sh` leading its own group, as `ssh` does, with one more
/// process in that group standing in for a `ProxyCommand`. Returns the shell
/// and the stand-in's id.
fn shell_with_proxy() -> (Child, u32) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "sleep 60 & echo $!; wait"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    (child, line.trim().parse().unwrap())
}

/// Dropping the guard ends the proxy too, which killing `ssh` alone leaves
/// running.
#[test]
fn ending_a_group_leader_ends_its_proxy_too() {
    let (child, proxy) = shell_with_proxy();
    assert!(alive(proxy));
    drop(ChildGuard(child));
    assert!(
        gone_within(proxy, Duration::from_secs(5)),
        "the proxy outlived its connection"
    );
}

/// Killing only the leader is what used to happen: the proxy survives it.
/// This proves the stand-in models the problem the group kill fixes.
#[test]
fn killing_only_the_leader_leaves_its_proxy_running() {
    let (mut child, proxy) = shell_with_proxy();
    child.kill().unwrap();
    child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(alive(proxy), "the stand-in should outlive a lone kill");
    let _ = Command::new("kill").arg(proxy.to_string()).status();
    assert!(gone_within(proxy, Duration::from_secs(5)));
}

/// A child that leads no group, such as one started without
/// `process_group`, is still killed and reaped.
#[test]
fn a_child_in_our_group_is_killed_and_reaped() {
    let mut child = Command::new("sleep").arg("60").spawn().unwrap();
    let id = child.id();
    end_process_group(&mut child);
    assert!(!alive(id));
    assert!(child.try_wait().unwrap().is_some());
}

/// A child already reaped is not signalled as a group again: its id may
/// since belong to someone else.
#[test]
fn a_reaped_child_is_left_alone() {
    let mut child = Command::new("true").process_group(0).spawn().unwrap();
    child.wait().unwrap();
    end_process_group(&mut child);
    assert!(child.try_wait().unwrap().is_some());
}
