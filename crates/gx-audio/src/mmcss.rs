#[cfg(any(windows, test))]
struct ScopedRegistration<H, F>
where
    F: FnOnce(H),
{
    cleanup: Option<(H, F)>,
}

#[cfg(any(windows, test))]
impl<H, F> ScopedRegistration<H, F>
where
    F: FnOnce(H),
{
    fn new(handle: H, revert: F) -> Self {
        Self {
            cleanup: Some((handle, revert)),
        }
    }
}

#[cfg(any(windows, test))]
impl<H, F> Drop for ScopedRegistration<H, F>
where
    F: FnOnce(H),
{
    fn drop(&mut self) {
        if let Some((handle, revert)) = self.cleanup.take() {
            revert(handle);
        }
    }
}

/// Registers the CPAL callback thread with Windows MMCSS on its first callback.
///
/// CPAL owns and drops the callback closure on its audio thread, so the scoped
/// registration is reverted on the same thread that created it.
#[derive(Default)]
pub(crate) struct AudioThreadPriority {
    #[cfg(windows)]
    state: RegistrationState,
}

impl AudioThreadPriority {
    #[inline]
    pub(crate) fn ensure_registered(&mut self) {
        #[cfg(windows)]
        if matches!(self.state, RegistrationState::Pending) {
            self.state = register_current_thread()
                .map(|registration| RegistrationState::Registered {
                    _registration: registration,
                })
                .unwrap_or(RegistrationState::Unavailable);
        }
    }
}

#[cfg(windows)]
type WindowsRegistration = ScopedRegistration<usize, fn(usize)>;

#[cfg(windows)]
#[derive(Default)]
enum RegistrationState {
    #[default]
    Pending,
    Registered {
        _registration: WindowsRegistration,
    },
    Unavailable,
}

#[cfg(windows)]
fn register_current_thread() -> Option<WindowsRegistration> {
    use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
    use windows::core::w;

    let mut task_index = 0;
    // "Audio" is the MMCSS task intended for regular low-latency playback.
    let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task_index) }.ok()?;
    Some(ScopedRegistration::new(
        handle.0 as usize,
        revert_registration,
    ))
}

#[cfg(windows)]
fn revert_registration(handle: usize) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::AvRevertMmThreadCharacteristics;

    let _ = unsafe { AvRevertMmThreadCharacteristics(HANDLE(handle as *mut _)) };
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::ScopedRegistration;

    #[test]
    fn scoped_registration_reverts_once_on_the_registration_thread() {
        let revert_count = Arc::new(AtomicUsize::new(0));
        let registration_thread = thread::current().id();

        {
            let revert_count = Arc::clone(&revert_count);
            let _registration = ScopedRegistration::new(7, move |handle| {
                assert_eq!(handle, 7);
                assert_eq!(thread::current().id(), registration_thread);
                revert_count.fetch_add(1, Ordering::Relaxed);
            });
        }

        assert_eq!(revert_count.load(Ordering::Relaxed), 1);
    }
}
