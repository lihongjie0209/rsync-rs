pub mod local;
pub mod ssh;
pub mod daemon;

pub use local::LocalTransport;
pub use ssh::SshTransport;
pub use daemon::DaemonClient;

/// Unified handle for either a local or SSH transport channel.
pub enum Transport {
    Local(LocalTransport),
    Ssh(SshTransport),
}
