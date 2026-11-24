mod desktop;
#[cfg(not(dev))]
mod frontend;
mod resource_limits;
mod startup;
mod tray;
mod update;

// mimalloc 在释放时主动向操作系统归还内存,避免 glibc 长期保留已释放的页。
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub fn run() -> std::process::ExitCode {
    if let Some(exit_code) = update::run_replacement_if_requested() {
        return exit_code;
    }
    desktop::run()
}
