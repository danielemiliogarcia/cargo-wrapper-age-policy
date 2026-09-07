//! Implements the age-policy plugin without duplicating Cargo's dependency resolver.

pub mod cargo_support;
pub mod config;
pub mod fallback;
pub mod policy;
pub mod protocol;

pub type Result<T> = std::result::Result<T, String>;
