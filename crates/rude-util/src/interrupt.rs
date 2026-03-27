use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

pub fn set_interrupted() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub fn install_handler() {
    if let Err(e) = ctrlc::set_handler(move || {
        set_interrupted();
        eprintln!("\nInterrupted. Cleaning up...");
    }) {
        eprintln!("Warning: Failed to set Ctrl+C handler: {e}");
    }
}
