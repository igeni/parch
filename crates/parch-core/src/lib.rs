//! Format-agnostic engine of the `parch` parallel archiver.
//!
//! The engine owns everything that does not depend on a concrete archive
//! format: the thread budget, filesystem scanning, the parallel compression
//! pipeline, parallel extraction and metadata handling. Formats plug in through
//! the traits in [`format`], compression methods through [`codec`].

pub mod budget;
pub mod codec;
pub mod create;
pub mod entry;
pub mod error;
pub mod extract;
pub mod filter;
pub mod format;
pub mod fsmeta;
pub mod report;
pub mod scan;
pub mod sink;
pub mod threads;
pub mod util;

pub use error::{Error, IoResultExt, Result};
