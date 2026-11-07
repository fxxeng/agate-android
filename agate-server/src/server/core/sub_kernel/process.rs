//! Host process capabilities used by the optional mihomo sidecar.
//!
//! This is deliberately narrower than a general platform service locator: it
//! owns only OS-specific process command setup, stale-PID termination, and
//! executable-bit handling. Android remains unsupported by the sidecar asset
//! matrix; sharing Unix mechanics does not imply Android binary compatibility.

use std::io;
use std::path::Path;
use std::process::Command as StdCommand;

use tokio::process::Command as TokioCommand;

pub(super) trait ProcessAdapter: Sync {
    fn configure_tokio_command(&self, command: &mut TokioCommand);
    fn kill_by_pid(&self, pid: u64) -> bool;
    fn set_executable(&self, path: &Path) -> io::Result<()>;
}

#[cfg(unix)]
struct HostProcessAdapter;

#[cfg(not(unix))]
struct HostProcessAdapter;

#[cfg(unix)]
impl ProcessAdapter for HostProcessAdapter {
    fn configure_tokio_command(&self, _command: &mut TokioCommand) {}

    fn kill_by_pid(&self, pid: u64) -> bool {
        StdCommand::new("kill")
            .args(["-9", &pid.to_string()])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn set_executable(&self, path: &Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
    }
}

#[cfg(not(unix))]
impl ProcessAdapter for HostProcessAdapter {
    fn configure_tokio_command(&self, _command: &mut TokioCommand) {}

    fn kill_by_pid(&self, _pid: u64) -> bool {
        false
    }

    fn set_executable(&self, _path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台不支持 Sidecar 进程管理",
        ))
    }
}

static HOST_PROCESS_ADAPTER: HostProcessAdapter = HostProcessAdapter;

pub(super) fn process_adapter() -> &'static dyn ProcessAdapter {
    &HOST_PROCESS_ADAPTER
}
