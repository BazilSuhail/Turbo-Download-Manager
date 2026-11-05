//! Clean-room download engine. The Iced UI lives in `main.rs` and only uses this crate's public API.

pub mod checksum;
pub mod engine;
pub mod extract;
pub mod filetype;
pub mod ftp;
pub mod grabber;
pub mod hosts;
pub mod ipc;
pub mod lang;
pub mod media;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod model;
pub mod native_host;
pub mod planner;
pub mod proxy;
pub mod register;
pub mod rss;
pub mod scan;
pub mod scheduler;
pub mod secrets;
pub mod serverhash;
pub mod store;
pub mod torrent_host;
pub mod torrent_policy;
pub mod torrent_service;
pub mod torrent_worker;
pub mod tools;
pub mod ui;

pub use engine::{Download, Error, Snapshot, Status, next_to_start};
pub use model::{Category, Column, ColumnSpec, Settings};
