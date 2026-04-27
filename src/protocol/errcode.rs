/// Exit codes returned by rsync (from errcode.h).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Ok = 0,
    Syntax = 1,
    Protocol = 2,
    FileSelect = 3,
    Unsupported = 4,
    StartClient = 5,
    SocketIo = 10,
    FileIo = 11,
    StreamIo = 12,
    MessageIo = 13,
    Ipc = 14,
    Crashed = 15,
    Terminated = 16,
    Signal1 = 19,
    Signal = 20,
    WaitChild = 21,
    Malloc = 22,
    Partial = 23,
    Vanished = 24,
    DelLimit = 25,
    Timeout = 30,
    ConTimeout = 35,
    CmdFailed = 124,
    CmdKilled = 125,
    CmdRun = 126,
    CmdNotFound = 127,
}

impl ExitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    pub fn description(self) -> &'static str {
        match self {
            ExitCode::Ok => "success",
            ExitCode::Syntax => "syntax or usage error",
            ExitCode::Protocol => "protocol incompatibility",
            ExitCode::FileSelect => "errors selecting input/output files",
            ExitCode::Unsupported => "requested action not supported",
            ExitCode::StartClient => "error starting client-server protocol",
            ExitCode::SocketIo => "error in socket IO",
            ExitCode::FileIo => "error in file IO",
            ExitCode::StreamIo => "error in rsync protocol data stream",
            ExitCode::MessageIo => "errors with program diagnostics",
            ExitCode::Ipc => "error in IPC code",
            ExitCode::Crashed => "sibling crashed",
            ExitCode::Terminated => "sibling terminated abnormally",
            ExitCode::Signal1 => "received SIGUSR1",
            ExitCode::Signal => "received SIGINT, SIGTERM, or SIGHUP",
            ExitCode::WaitChild => "error in waitpid()",
            ExitCode::Malloc => "error allocating core memory buffers",
            ExitCode::Partial => "partial transfer",
            ExitCode::Vanished => "file(s) vanished on sender side",
            ExitCode::DelLimit => "skipped some deletes due to --max-delete",
            ExitCode::Timeout => "timeout in data send/receive",
            ExitCode::ConTimeout => "timeout waiting for daemon connection",
            ExitCode::CmdFailed => "remote command failed",
            ExitCode::CmdKilled => "remote command killed",
            ExitCode::CmdRun => "remote command could not be run",
            ExitCode::CmdNotFound => "remote command not found",
        }
    }
}

impl std::fmt::Display for ExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.as_i32(), self.description())
    }
}
