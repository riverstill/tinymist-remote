//! Sampling of the resource usage and connection status of the services.

use std::collections::HashMap;
use std::time::Duration;

use sysinfo::{Pid, ProcessesToUpdate, System};

use super::supervisor::{ServiceStatus, SharedServices};

/// Samples the resource usage of the services once per second.
pub fn spawn_sampler(shared: SharedServices) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("tinymist-manager-status".to_string())
        .spawn(move || {
            let mut system = System::new();
            loop {
                sample(&mut system, &shared);
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .expect("failed to spawn the status sampler thread")
}

/// Samples the CPU usage, memory usage and connection count of the services.
fn sample(system: &mut System, shared: &SharedServices) {
    let Ok(mut services) = shared.lock() else {
        return;
    };

    let pids: Vec<Pid> = services
        .iter()
        .filter_map(|it| it.pid.map(Pid::from_u32))
        .collect();
    if !pids.is_empty() {
        system.refresh_processes(ProcessesToUpdate::Some(&pids), true);
    }

    let ports: Vec<(u16, u32)> = services
        .iter()
        .filter(|it| it.status == ServiceStatus::Running)
        .filter_map(|it| it.pid.map(|pid| (it.entry.port, pid)))
        .collect();
    let connections = count_established_connections(&ports);

    for service in services.iter_mut() {
        let Some(pid) = service.pid else {
            service.cpu_percent = 0.0;
            service.memory_mb = 0.0;
            service.connections = 0;
            continue;
        };

        if let Some(process) = system.process(Pid::from_u32(pid)) {
            service.cpu_percent = process.cpu_usage();
            service.memory_mb = process.memory() as f64 / 1024.0 / 1024.0;
        }
        service.connections = connections
            .get(&(service.entry.port, pid))
            .copied()
            .unwrap_or(0);
    }
}

/// Counts the established TCP connections to the given listening ports.
///
/// The returned map is keyed by `(port, pid)` so that multiple services
/// listening on different ports do not interfere with each other.
fn count_established_connections(ports: &[(u16, u32)]) -> HashMap<(u16, u32), u32> {
    use windows_sys::Win32::Foundation::NO_ERROR;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP_STATE_ESTAB, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    let mut result = HashMap::new();
    if ports.is_empty() {
        return result;
    }
    let wanted: std::collections::HashSet<(u16, u32)> = ports.iter().copied().collect();

    // SAFETY: the buffer is either null (to query the required size) or sized
    // according to the value returned by the first call.
    unsafe {
        let mut size = 0u32;
        let err = GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if err != NO_ERROR && size == 0 {
            return result;
        }

        let mut buffer = vec![0u8; size as usize];
        let err = GetExtendedTcpTable(
            buffer.as_mut_ptr().cast(),
            &mut size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if err != NO_ERROR {
            return result;
        }

        let table = &*(buffer.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        for i in 0..table.dwNumEntries as usize {
            let row = *table.table.as_ptr().add(i);
            if row.dwState != MIB_TCP_STATE_ESTAB as u32 {
                continue;
            }
            let local_port = u16::from_be((row.dwLocalPort & 0xffff) as u16);
            let key = (local_port, row.dwOwningPid);
            if wanted.contains(&key) {
                *result.entry(key).or_insert(0) += 1;
            }
        }
    }

    result
}
