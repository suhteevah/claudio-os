//! Combined serial + framebuffer logger using the `log` crate.
//!
//! Also feeds log lines into the kernel log ring buffer for virtual console 6.

extern crate alloc;
use alloc::format;

struct KernelLogger;

impl log::Log for KernelLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            crate::serial_println!("[{:5}] {}", record.level(), record.args());
            // Also push into the kernel log ring buffer for vconsole 6.
            let line = format!("[{:5}] {}", record.level(), record.args());
            crate::vconsole::push_kernel_log(&line);
        }
    }

    fn flush(&self) {}
}

static LOGGER: KernelLogger = KernelLogger;

pub fn init() {
    log::set_logger(&LOGGER).expect("logger already initialized");
    log::set_max_level(log::LevelFilter::Trace);
}
