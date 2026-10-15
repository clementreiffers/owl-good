//! Native AppKit presentation; all Cocoa objects stay on the main thread.
#[cfg(target_os = "macos")]
mod icons;
#[cfg(target_os = "macos")]
mod native;
mod policy;
#[cfg(target_os = "macos")]
pub use native::run;

#[cfg(not(target_os = "macos"))]
pub fn run(_: isdown_core::Config, _: std::path::PathBuf) -> Result<(), String> {
    Err("isDown requires macOS 13 or newer".into())
}
