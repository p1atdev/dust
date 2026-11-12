//! Disk usage scanning for native applications, with explicit cancellation and errors.
//!
//! The `scan` API retains every directory entry and distinguishes unknown sizes
//! from zero. The original terminal interface continues to use the CLI modules.

#[doc(hidden)]
pub mod cli;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod dir_walker;
#[doc(hidden)]
pub mod display;
#[doc(hidden)]
pub mod display_node;
#[doc(hidden)]
pub mod filter;
#[doc(hidden)]
pub mod filter_type;
#[doc(hidden)]
pub mod node;
#[doc(hidden)]
pub mod platform;
#[doc(hidden)]
pub mod progress;
pub mod scanner;
#[doc(hidden)]
pub mod utils;
#[doc(hidden)]
pub use progress::PIndicator;
pub use scanner::*;
