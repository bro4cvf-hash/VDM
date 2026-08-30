//! module hub; Manager lives in downloader.rs per spec layout
pub mod browser;
pub mod downloader;
pub mod file_allocator;
pub mod probe;
pub mod rate_limiter;
pub mod server;
pub mod sys_icon;
pub mod worker;
pub mod ytdl;

pub use downloader::{Manager, TaskSnapshot};
