//! CPU and memory of each host the window lists, for the status bar and the
//! sidebar's host rows. Every host has its own worker thread sampling it every
//! couple of seconds: this machine through `sysinfo`, a remote host over one
//! SSH shell kept open between samples. The UI thread only starts and stops
//! workers and takes their samples on later ticks, so a slow or unreachable
//! host never blocks rendering or delays another host.

mod render;
mod sample;

#[cfg(test)]
mod tests;

use crate::{
    Error, Result,
    pace::{Intervals, Pace, Schedule},
    usage::{Host, Shell},
};
pub(crate) use render::{
    CPU_WARN, DISK_WARN, MEMORY_WARN, gauges, line, severity, storage, tooltip, uptime,
};
// The control socket, Unix only, reads samples too.
#[cfg(unix)]
pub(crate) use sample::Sample;
#[cfg(not(unix))]
use sample::Sample;
use sample::{Disk, Memory, Os, Ticks};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

/// The status bar's CPU figure is worth refreshing often only while the user
/// works in the window; a covered window does not need it at all.
const INTERVALS: Intervals = Intervals {
    active: Duration::from_secs(2),
    shown: Duration::from_secs(5),
    hidden: Duration::from_secs(30),
};
/// A host that could not be reached is tried again after this long.
const RETRY: Duration = Duration::from_secs(30);
/// macOS hosts spend one second of this measuring CPU.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// CPU samples kept for the sparklines.
pub(crate) const HISTORY: usize = 16;
/// Hosts are few; an unbounded endpoint list still cannot start more workers.
const HOST_LIMIT: usize = 16;

/// Where samples come from.
enum Source {
    Local {
        /// Boxed: far larger than a remote shell's state.
        system: Box<sysinfo::System>,
        /// CPU usage is measured between two refreshes, so the first has none.
        primed: bool,
        disk: HomeDisk,
    },
    Remote {
        shell: Shell,
        os: Os,
        ticks: Option<Ticks>,
    },
}

impl Source {
    fn open(host: &Host) -> Result<Self> {
        match host {
            Host::Local => Ok(Self::Local {
                system: Box::new(sysinfo::System::new()),
                primed: false,
                disk: HomeDisk::open(),
            }),
            Host::Ssh(target) => {
                let mut shell = Shell::connect(target).map_err(remote)?;
                let uname = shell.run("uname -s", STEP_TIMEOUT).map_err(remote)?;
                let os = Os::from_uname(&uname.stdout).ok_or_else(|| {
                    Error::SystemLoadUnsupported(uname.stdout.trim().chars().take(32).collect())
                })?;
                Ok(Self::Remote {
                    shell,
                    os,
                    ticks: None,
                })
            }
            Host::Wsl(_) => Err(Error::WslHostUnsupported),
        }
    }

    fn sample(&mut self) -> Result<Sample> {
        match self {
            Self::Local {
                system,
                primed,
                disk,
            } => {
                system.refresh_cpu_usage();
                system.refresh_memory();
                let cpu = std::mem::replace(primed, true)
                    .then(|| system.global_cpu_usage().clamp(0., 100.));
                let load = sysinfo::System::load_average();
                Ok(Sample {
                    cpu,
                    memory: Some(Memory {
                        used: system.used_memory(),
                        total: system.total_memory(),
                    })
                    .filter(|memory| memory.total > 0),
                    cores: u32::try_from(system.cpus().len())
                        .ok()
                        .filter(|cores| *cores > 0),
                    // Windows keeps no load average; sysinfo reports zeros.
                    load: (!cfg!(windows)).then_some([
                        load.one as f32,
                        load.five as f32,
                        load.fifteen as f32,
                    ]),
                    disk: disk.sample(),
                    uptime: Some(sysinfo::System::uptime()).filter(|seconds| *seconds > 0),
                })
            }
            Self::Remote { shell, os, ticks } => {
                let output = shell.run(os.command(), STEP_TIMEOUT).map_err(remote)?;
                let answer = sample::parse(&output.stdout, *ticks)?;
                *ticks = answer.ticks;
                Ok(answer.sample)
            }
        }
    }
}

/// This machine's volume holding the home directory, found once: listing every
/// mount on each sample would also touch slow network mounts.
struct HomeDisk {
    disks: sysinfo::Disks,
    index: Option<usize>,
}

impl HomeDisk {
    fn open() -> Self {
        let disks = sysinfo::Disks::new_with_refreshed_list_specifics(
            sysinfo::DiskRefreshKind::nothing().with_storage(),
        );
        let index = crate::config::home().ok().and_then(|home| {
            home_volume(&home, disks.list().iter().map(sysinfo::Disk::mount_point))
        });
        Self { disks, index }
    }

    fn sample(&mut self) -> Option<Disk> {
        let disk = self.disks.list_mut().get_mut(self.index?)?;
        disk.refresh_specifics(sysinfo::DiskRefreshKind::nothing().with_storage());
        Some(Disk {
            available: disk.available_space().min(disk.total_space()),
            total: disk.total_space(),
        })
        .filter(|disk| disk.total > 0)
    }
}

/// The index of the mount point holding `home`: the longest one it lies under.
fn home_volume<'a>(
    home: &std::path::Path,
    mounts: impl IntoIterator<Item = &'a std::path::Path>,
) -> Option<usize> {
    mounts
        .into_iter()
        .enumerate()
        .filter(|(_, mount)| home.starts_with(mount))
        .max_by_key(|(_, mount)| mount.as_os_str().len())
        .map(|(index, _)| index)
}

fn remote(error: Error) -> Error {
    Error::SystemLoadRemote(Box::new(error))
}

struct Worker {
    schedule: Arc<Schedule>,
    results: mpsc::Receiver<Result<Sample>>,
}

impl Drop for Worker {
    /// The thread ends after its current sample; nothing waits for it here.
    fn drop(&mut self) {
        self.schedule.stop();
    }
}

fn spawn(host: Host, pace: Pace) -> Option<Worker> {
    let schedule = Arc::new(Schedule::new(pace));
    let (sender, results) = mpsc::sync_channel(4);
    let shared = schedule.clone();
    let spawned = thread::Builder::new()
        .name("herdr-system-load".into())
        .spawn(move || run(&host, &shared, &sender));
    match spawned {
        Ok(_) => Some(Worker { schedule, results }),
        Err(error) => {
            tracing::warn!(category = "system-load", %error, "could not start a CPU and memory worker");
            None
        }
    }
}

fn run(host: &Host, schedule: &Schedule, results: &mpsc::SyncSender<Result<Sample>>) {
    let mut source: Option<Source> = None;
    loop {
        let opened = match source.take() {
            Some(source) => Ok(source),
            None => Source::open(host),
        };
        let result = opened.and_then(|mut opened| {
            let sample = opened.sample();
            // A failed remote step leaves the shell unusable; reconnect.
            if sample.is_ok() || matches!(opened, Source::Local { .. }) {
                source = Some(opened);
            }
            sample
        });
        let succeeded = result.is_ok();
        let finished = Instant::now();
        match results.try_send(result) {
            // The UI is behind; it will take the next one.
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => return,
        }
        let interval = |pace| if succeeded { INTERVALS.at(pace) } else { RETRY };
        if !schedule.wait(finished, interval) {
            return;
        }
    }
}

/// One host's samples: the latest, and recent CPU usage for a sparkline.
#[derive(Default)]
pub(crate) struct Reading {
    latest: Option<Sample>,
    /// Recent CPU usage, oldest first.
    history: VecDeque<f32>,
    /// Why the latest sample failed; the last good one stays shown.
    error: Option<String>,
}

impl Reading {
    pub fn latest(&self) -> Option<&Sample> {
        self.latest.as_ref()
    }

    pub fn history(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.history.iter().copied()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn apply(&mut self, result: Result<Sample>) {
        match result {
            Ok(sample) => {
                if let Some(cpu) = sample.cpu {
                    if self.history.len() == HISTORY {
                        self.history.pop_front();
                    }
                    self.history.push_back(cpu);
                }
                self.latest = Some(sample);
                self.error = None;
            }
            Err(error) => {
                // The display boundary: the cause says what went wrong remotely.
                let mut text = error.to_string();
                let mut source = std::error::Error::source(&error);
                while let Some(cause) = source {
                    text.push(' ');
                    text.push_str(&cause.to_string());
                    source = cause.source();
                }
                self.error = Some(text);
            }
        }
    }
}

#[derive(Default)]
struct Monitor {
    /// None once its thread could not start or has ended.
    worker: Option<Worker>,
    reading: Reading,
}

/// Every sampled host's reading, keyed by host.
#[derive(Default)]
pub(crate) struct SystemLoad {
    monitors: HashMap<Host, Monitor>,
}

impl SystemLoad {
    pub fn get(&self, host: &Host) -> Option<&Reading> {
        self.monitors.get(host).map(|monitor| &monitor.reading)
    }

    /// Samples exactly `hosts` (at most [`HOST_LIMIT`]) at `pace`: starts
    /// workers for new ones, stops and forgets the rest, and takes finished
    /// samples. Returns whether anything shown changed.
    pub fn poll(&mut self, hosts: impl IntoIterator<Item = Host>, pace: Pace) -> bool {
        let wanted: Vec<Host> = hosts.into_iter().take(HOST_LIMIT).collect();
        let before = self.monitors.len();
        self.monitors.retain(|host, _| wanted.contains(host));
        let mut changed = self.monitors.len() != before;
        for host in wanted {
            self.monitors
                .entry(host)
                .or_insert_with_key(|host| Monitor {
                    worker: spawn(host.clone(), pace),
                    reading: Reading::default(),
                });
        }
        for monitor in self.monitors.values_mut() {
            if let Some(worker) = &monitor.worker {
                worker.schedule.set_pace(pace);
            }
            while let Some(worker) = &monitor.worker {
                match worker.results.try_recv() {
                    Ok(result) => {
                        monitor.reading.apply(result);
                        changed = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => monitor.worker = None,
                }
            }
        }
        changed
    }
}
