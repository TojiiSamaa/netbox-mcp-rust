//! nbmcp : serveur MCP pour NetBox (lecture, écriture gardée, plugins).
#![allow(clippy::result_large_err)] // NbError porte un message, un conseil et des détails : il est rare et gros.

pub mod client;
pub mod config;
pub mod error;
pub mod filters;
pub mod http;
pub mod log;
pub mod mock;
pub mod openapi;
pub mod policy;
pub mod redact;
pub mod runtime;
pub mod schema;
pub mod server;
pub mod stats;
pub mod tool;
pub mod tools;
pub mod types;
