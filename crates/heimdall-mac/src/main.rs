//! Heimdall for macOS: the same launcher, drawn with AppKit on top of the
//! system's own glass. Builds to a stub on other platforms so the workspace
//! still checks everywhere.

#[cfg(target_os = "macos")]
mod mac;

fn main() {
    #[cfg(target_os = "macos")]
    mac::run();
    #[cfg(not(target_os = "macos"))]
    eprintln!("Heimdall for macOS only runs on macOS.");
}
