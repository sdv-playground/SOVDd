//! SocketCAN transport adapter (Linux only)

#[cfg(target_os = "linux")]
mod adapter;
#[cfg(target_os = "linux")]
pub mod scanner;

#[cfg(target_os = "linux")]
pub use adapter::SocketCanAdapter;
