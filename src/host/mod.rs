//! Native host boundary modules.

#[cfg(feature = "host-abi-v2")]
pub mod abi;
#[cfg(feature = "legacy")]
pub mod events;
#[cfg(feature = "legacy")]
pub mod files;
pub mod frame;
pub mod input;
#[cfg(feature = "legacy")]
pub(crate) mod logging;
pub mod media;
pub mod protocol;
#[cfg(feature = "legacy")]
mod provider;
pub mod runtime;
pub mod script_io;
#[cfg(feature = "legacy")]
mod session;

#[cfg(feature = "legacy")]
pub use provider::ResourceProvider;
#[cfg(feature = "legacy")]
pub use session::HostSession;
#[cfg(feature = "legacy")]
pub(crate) use session::current_session;
