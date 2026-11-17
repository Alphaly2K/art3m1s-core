//! Native host boundary modules.

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
mod provider;
pub mod renderer;
pub mod runtime;
pub mod script_io;
#[cfg(feature = "legacy")]
mod session;

pub use provider::ResourceProvider;
#[cfg(feature = "legacy")]
pub use session::HostSession;
#[cfg(feature = "legacy")]
pub(crate) use session::current_session;
