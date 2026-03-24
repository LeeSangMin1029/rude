//! Global interrupt flag for graceful shutdown.
//!
//! Each binary installs its own Ctrl+C handler that calls `set_interrupted()`.
//! Library code checks `is_interrupted()` to cooperate with shutdown.

use std::sync::atomic::{AtomicBool, Ordering};

/// Global flag for Ctrl+C handling.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Check if an interrupt has been signaled.
pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

/// Signal an interrupt (called from Ctrl+C handlers).
pub fn set_interrupted() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Install Ctrl+C handler that sets the interrupt flag.
///
/// Shared by all binaries.
pub fn install_handler() {
    if let Err(e) = ctrlc::set_handler(move || {
        set_interrupted();
        eprintln!("\nInterrupted. Cleaning up...");
    }) {
        eprintln!("Warning: Failed to set Ctrl+C handler: {e}");
    }
}
