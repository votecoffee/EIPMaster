use std::{
    io,
    sync::atomic::{AtomicBool, Ordering::Relaxed},
};

static REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};

#[cfg(windows)]
unsafe extern "system" fn handle(event: u32) -> i32 {
    if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
        REQUESTED.store(true, Relaxed);
        1
    } else {
        0
    }
}

pub(super) fn install() -> io::Result<()> {
    // SAFETY: The handler has process lifetime and performs only an atomic store.
    #[cfg(windows)]
    if unsafe { SetConsoleCtrlHandler(Some(handle), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn requested() -> bool {
    REQUESTED.load(Relaxed)
}
