use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

pub fn set_interrupted() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Install Ctrl+C handler that sets the interrupt flag.
/// Shared by all binaries that need graceful shutdown.
pub fn install_handler() {
    if let Err(e) = ctrlc::set_handler(move || {
        set_interrupted();
        eprintln!("\nInterrupted. Cleaning up...");
    }) {
        eprintln!("Warning: Failed to set Ctrl+C handler: {e}");
    }
}
