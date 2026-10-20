//! The supervisor keeping the language server processes alive.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::config::ServiceEntry;

/// The shared view of the services read by the graphical interface.
pub type SharedServices = Arc<Mutex<Vec<ServiceView>>>;

/// The runtime status of a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    /// The service is not running.
    Stopped,
    /// The service process is alive.
    Running,
    /// The service exited unexpectedly and is scheduled to restart.
    Restarting,
}

/// The state of a service observed by the interface.
#[derive(Debug, Clone)]
pub struct ServiceView {
    /// The persisted configuration of the service.
    pub entry: ServiceEntry,
    /// The runtime status.
    pub status: ServiceStatus,
    /// The process identifier of the language server, if running.
    pub pid: Option<u32>,
    /// The CPU usage in percent sampled in the last interval.
    pub cpu_percent: f32,
    /// The resident memory usage in megabytes.
    pub memory_mb: f64,
    /// The number of established TCP connections to the listening port.
    pub connections: u32,
    /// The number of restart attempts after unexpected exits.
    pub restarts: u32,
}

impl ServiceView {
    /// The display address of the service, e.g. `ws://0.0.0.0:23626`.
    pub fn endpoint(&self) -> String {
        format!(
            "{}://0.0.0.0:{}",
            self.entry.transport.display(),
            self.entry.port
        )
    }
}

/// A command sent to the supervisor thread.
#[derive(Debug, Clone)]
pub enum ServiceCommand {
    /// Starts the service with the given name.
    Start(String),
    /// Stops the service with the given name.
    Stop(String),
    /// Restarts the service with the given name.
    Restart(String),
    /// Reconciles the supervisor with the edited configuration.
    ApplyConfig(Vec<ServiceEntry>),
    /// Starts all the services.
    StartAll,
    /// Stops all the services.
    StopAll,
}

/// A supervised child process.
struct ServiceSlot {
    entry: ServiceEntry,
    child: Option<Child>,
    manual_stop: bool,
    restart_at: Option<Instant>,
    restarts: u32,
    started_at: Option<Instant>,
}

/// The supervisor running on a dedicated thread.
pub struct Supervisor {
    slots: HashMap<String, ServiceSlot>,
    order: Vec<String>,
    shared: SharedServices,
    receiver: mpsc::Receiver<ServiceCommand>,
    log_dir: Option<PathBuf>,
    /// The name of the service to start after the current poll.
    pending_start: Vec<String>,
}

/// The delays between restart attempts, in seconds.
const RESTART_DELAYS: [u64; 4] = [1, 2, 5, 10];

/// The running time after which the restart counter is reset.
const RESTART_RESET: Duration = Duration::from_secs(60);

impl Supervisor {
    /// Spawns the supervisor thread.
    pub fn spawn(
        shared: SharedServices,
        receiver: mpsc::Receiver<ServiceCommand>,
        services: Vec<ServiceEntry>,
    ) -> std::thread::JoinHandle<()> {
        let log_dir = super::config::log_dir();
        let mut supervisor = Supervisor {
            slots: HashMap::new(),
            order: Vec::new(),
            shared,
            receiver,
            log_dir,
            pending_start: Vec::new(),
        };
        supervisor.apply_config(services);
        std::thread::Builder::new()
            .name("tinymist-manager-supervisor".to_string())
            .spawn(move || supervisor.run())
            .expect("failed to spawn the supervisor thread")
    }

    /// The main loop of the supervisor.
    fn run(mut self) {
        loop {
            let cmd = self.receiver.recv_timeout(Duration::from_millis(200)).ok();
            self.handle(cmd);
            self.poll();
            self.publish();
        }
    }

    fn handle(&mut self, cmd: Option<ServiceCommand>) {
        let Some(cmd) = cmd else {
            return;
        };
        match cmd {
            ServiceCommand::Start(name) => self.start_service(&name),
            ServiceCommand::Stop(name) => self.stop_service(&name),
            ServiceCommand::Restart(name) => {
                self.stop_service(&name);
                self.start_service(&name);
            }
            ServiceCommand::StartAll => {
                let names: Vec<String> = self.order.clone();
                for name in names {
                    self.start_service(&name);
                }
            }
            ServiceCommand::StopAll => {
                let names: Vec<String> = self.order.clone();
                for name in names {
                    self.stop_service(&name);
                }
            }
            ServiceCommand::ApplyConfig(services) => self.apply_config(services),
        }
    }

    /// Reconciles the slots with the edited configuration.
    fn apply_config(&mut self, services: Vec<ServiceEntry>) {
        let kept: Vec<String> = services.iter().map(|it| it.name.clone()).collect();

        // Stops and drops the removed services.
        for name in self.order.clone() {
            if !kept.contains(&name) {
                self.stop_service(&name);
                self.slots.remove(&name);
            }
        }

        for entry in services {
            let changed = match self.slots.get(&entry.name) {
                Some(slot) => slot.entry != entry,
                None => false,
            };

            if changed {
                // Stops the process of the outdated configuration, so that the
                // child handle is not lost.
                self.stop_service(&entry.name);
                self.slots.insert(
                    entry.name.clone(),
                    ServiceSlot {
                        entry,
                        child: None,
                        manual_stop: true,
                        restart_at: None,
                        restarts: 0,
                        started_at: None,
                    },
                );
            } else if !self.slots.contains_key(&entry.name) {
                self.slots.insert(
                    entry.name.clone(),
                    ServiceSlot {
                        entry,
                        child: None,
                        manual_stop: true,
                        restart_at: None,
                        restarts: 0,
                        started_at: None,
                    },
                );
            }
        }

        self.order = kept;
        self.pending_start.retain(|name| self.order.contains(name));
    }

    /// Starts (or restarts) the service with the given name.
    fn start_service(&mut self, name: &str) {
        let entry = {
            let Some(slot) = self.slots.get(name) else {
                return;
            };
            if slot.child.is_some() {
                return;
            }
            slot.entry.clone()
        };

        let Some(child) = self.spawn_child(&entry) else {
            log::error!("failed to spawn the service {name}");
            return;
        };
        log::info!("started the service {name} (pid {})", child.id());
        if let Some(slot) = self.slots.get_mut(name) {
            slot.child = Some(child);
            slot.manual_stop = false;
            slot.restart_at = None;
            slot.started_at = Some(Instant::now());
        }
    }

    /// Stops the service with the given name.
    fn stop_service(&mut self, name: &str) {
        let Some(slot) = self.slots.get_mut(name) else {
            return;
        };
        slot.manual_stop = true;
        slot.restart_at = None;
        if let Some(mut child) = slot.child.take() {
            log::info!("stopping the service {name}");
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Spawns the language server process for the given entry.
    fn spawn_child(&self, entry: &ServiceEntry) -> Option<Child> {
        let exe = std::env::current_exe().ok()?;
        let log_path = self.log_path(&entry.name)?;
        if let Some(dir) = log_path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        rotate_log(&log_path);

        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok()?;
        let log_err = log.try_clone().ok()?;

        let mut cmd = Command::new(exe);
        cmd.args(entry.server_args())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .stdin(Stdio::null());

        match cmd.spawn() {
            Ok(child) => Some(child),
            Err(err) => {
                log::error!("failed to spawn the service {}: {err}", entry.name);
                None
            }
        }
    }

    fn log_path(&self, name: &str) -> Option<PathBuf> {
        let name = name.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
        Some(self.log_dir.as_ref()?.join(format!("{name}.log")))
    }

    /// Checks the child processes and schedules restarts.
    fn poll(&mut self) {
        for name in self.order.clone() {
            let Some(slot) = self.slots.get_mut(&name) else {
                continue;
            };

            if let Some(child) = slot.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let runtime = slot.started_at.map(|at| at.elapsed());
                        log::info!("the service {name} exited with {status}");
                        slot.child = None;
                        slot.started_at = None;

                        if slot.manual_stop {
                            continue;
                        }
                        // A service that kept running for a while is stable, so
                        // the restart counter is reset.
                        if runtime.is_some_and(|t| t >= RESTART_RESET) {
                            slot.restarts = 0;
                        }
                        let delay = RESTART_DELAYS
                            [slot.restarts.min(RESTART_DELAYS.len() as u32 - 1) as usize];
                        slot.restarts += 1;
                        slot.restart_at = Some(Instant::now() + Duration::from_secs(delay));
                        log::info!("the service {name} is scheduled to restart in {delay}s");
                    }
                    Ok(None) => {}
                    Err(err) => {
                        log::error!("failed to poll the service {name}: {err}");
                        slot.child = None;
                    }
                }
            }

            if slot.child.is_none()
                && !slot.manual_stop
                && slot.restart_at.is_some_and(|at| Instant::now() >= at)
            {
                slot.restart_at = None;
                self.pending_start.push(name.clone());
            }
        }

        for name in std::mem::take(&mut self.pending_start) {
            self.start_service(&name);
        }
    }

    /// Publishes the current view to the shared state.
    fn publish(&self) {
        let mut views = Vec::new();
        for name in &self.order {
            let Some(slot) = self.slots.get(name) else {
                continue;
            };
            views.push(ServiceView {
                entry: slot.entry.clone(),
                status: if slot.child.is_some() {
                    ServiceStatus::Running
                } else if slot.restart_at.is_some() {
                    ServiceStatus::Restarting
                } else {
                    ServiceStatus::Stopped
                },
                pid: slot.child.as_ref().map(|child| child.id()),
                cpu_percent: 0.0,
                memory_mb: 0.0,
                connections: 0,
                restarts: slot.restarts,
            });
        }
        *self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = views;
    }
}

/// Renames the log file when it grows too large.
fn rotate_log(path: &PathBuf) {
    const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() < MAX_LOG_SIZE {
        return;
    }
    let mut old = path.clone().into_os_string();
    old.push(".old");
    let _ = fs::rename(path, PathBuf::from(old));
}
