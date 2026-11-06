use std::time::Duration;

#[cfg(windows)]
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, SYNCHRONIZATION_SYNCHRONIZE,
        SetWaitableTimer, TIMER_MODIFY_STATE, WaitForSingleObject,
    },
};

/// One wait handle for the lifetime of the cyclic producer.
pub(crate) struct DeadlineWait {
    #[cfg(windows)]
    timer: Option<OwnedHandle>,
}

impl DeadlineWait {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(windows)]
            timer: {
                // SAFETY: The null pointers select an unnamed, non-inheritable timer.
                let handle = unsafe {
                    CreateWaitableTimerExW(
                        ptr::null(),
                        ptr::null(),
                        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                        SYNCHRONIZATION_SYNCHRONIZE | TIMER_MODIFY_STATE,
                    )
                };
                // SAFETY: The API transfers ownership of every non-null handle.
                (!handle.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(handle) })
            },
        }
    }

    /// The caller limits waits to 10 ms and rechecks its monotonic deadline after each wait.
    pub(crate) fn wait(&mut self, duration: Duration) {
        #[cfg(windows)]
        if let Some(timer) = &self.timer {
            let handle = timer.as_raw_handle();
            let due = -(duration.as_nanos().div_ceil(100).max(1) as i64);
            // SAFETY: We own the timer and the due time is live for the call.
            // A negative due time requests a relative one-shot in 100 ns units.
            let signaled = unsafe {
                SetWaitableTimer(handle, &due, 0, None, ptr::null(), 0) != 0
                    && WaitForSingleObject(handle, 50) == WAIT_OBJECT_0
            };
            if !signaled {
                // Return to the deadline loop immediately. Subsequent waits use the portable path.
                self.timer = None;
            }
            return;
        }
        std::thread::sleep(duration);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn one_shot_rearms_on_the_same_handle() {
        let mut waiter = DeadlineWait::new();
        let Some(handle) = waiter.timer.as_ref().map(AsRawHandle::as_raw_handle) else {
            return; // High-resolution timers require Windows 10 version 1803 or later.
        };
        for _ in 0..3 {
            let before = std::time::Instant::now();
            waiter.wait(Duration::from_millis(2));
            assert!(before.elapsed() >= Duration::from_millis(1));
            assert_eq!(
                waiter.timer.as_ref().map(AsRawHandle::as_raw_handle),
                Some(handle)
            );
        }
    }
}
