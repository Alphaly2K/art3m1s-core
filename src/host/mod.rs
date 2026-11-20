//! Native host boundary modules.

#[cfg(feature = "host-abi-v2")]
mod abi;
#[cfg(not(feature = "legacy"))]
pub mod api;
#[cfg(feature = "legacy")]
pub mod events;
#[cfg(feature = "legacy")]
pub mod files;
#[cfg(any(test, not(feature = "legacy"), feature = "host-abi-v2"))]
pub(crate) mod frame;
pub(crate) mod input;
#[cfg(feature = "legacy")]
pub(crate) mod logging;
pub mod media;
#[cfg(any(test, not(feature = "legacy"), feature = "host-abi-v2"))]
mod protocol;
#[cfg(feature = "legacy")]
mod provider;
#[cfg(any(test, not(feature = "legacy"), feature = "host-abi-v2"))]
pub(crate) mod runtime;
#[cfg(any(test, not(feature = "legacy"), feature = "host-abi-v2"))]
mod script_io;
#[cfg(feature = "legacy")]
mod session;

#[cfg(feature = "legacy")]
pub use provider::ResourceProvider;
#[cfg(feature = "legacy")]
pub use session::HostSession;
#[cfg(feature = "legacy")]
pub(crate) use session::{TextInjectResult, current_session};
