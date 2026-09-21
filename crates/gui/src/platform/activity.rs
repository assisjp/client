//! Whether this front end is actually the application receiving input.
//!
//! A GPUI window can remain its application's active window on macOS while a
//! different application is frontmost. A read receipt needs both facts.

/// Whether this application is currently active at the operating-system level.
pub fn application_is_active() -> bool {
    imp::application_is_active()
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    pub(super) fn application_is_active() -> bool {
        let Some(main_thread) = MainThreadMarker::new() else {
            log::warn!("application activity was queried away from the main thread");
            return false;
        };
        NSApplication::sharedApplication(main_thread).isActive()
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// GPUI's window-active signal is sufficient away from macOS.
    pub(super) const fn application_is_active() -> bool {
        true
    }
}
