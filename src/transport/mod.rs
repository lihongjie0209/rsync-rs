pub mod local;
pub mod ssh;

pub use local::LocalTransport;
pub use ssh::SshTransport;

/// Unified handle for either a local or SSH transport channel.
pub enum Transport {
    Local(LocalTransport),
    Ssh(SshTransport),
}
