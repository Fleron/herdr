use std::ptr::NonNull;

#[repr(C)]
struct TisInputSource {
    _private: [u8; 0],
}

type TisInputSourceRef = *const TisInputSource;
type CfTypeRef = *const libc::c_void;
type CfStringRef = *const libc::c_void;
type OsStatus = libc::c_int;
type Boolean = libc::c_uchar;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    #[link_name = "kTISPropertyInputSourceID"]
    static TIS_PROPERTY_INPUT_SOURCE_ID: CfStringRef;

    #[link_name = "TISCopyCurrentKeyboardInputSource"]
    fn tis_copy_current_keyboard_input_source() -> TisInputSourceRef;

    #[link_name = "TISCopyCurrentASCIICapableKeyboardLayoutInputSource"]
    fn tis_copy_current_ascii_capable_keyboard_layout_input_source() -> TisInputSourceRef;

    #[link_name = "TISGetInputSourceProperty"]
    fn tis_get_input_source_property(
        input_source: TisInputSourceRef,
        property_key: CfStringRef,
    ) -> CfTypeRef;

    #[link_name = "TISSelectInputSource"]
    fn tis_select_input_source(input_source: TisInputSourceRef) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    #[link_name = "CFRelease"]
    fn cf_release(value: CfTypeRef);

    #[link_name = "CFEqual"]
    fn cf_equal(left: CfTypeRef, right: CfTypeRef) -> Boolean;

    #[link_name = "kCFRunLoopDefaultMode"]
    static CF_RUN_LOOP_DEFAULT_MODE: CfStringRef;

    #[link_name = "CFRunLoopRunInMode"]
    fn cf_run_loop_run_in_mode(
        mode: CfStringRef,
        seconds: f64,
        return_after_source_handled: Boolean,
    ) -> libc::c_int;
}

/// Pump the main thread's run loop once (non-blocking) so the process receives the
/// `kTISNotifySelectedKeyboardInputSourceChanged` notification and refreshes the per-process cache
/// that `TISCopyCurrentKeyboardInputSource` reads. That notification arrives only via the main
/// thread's run loop, so a process that never runs a CFRunLoop (the headless server) reads a stale
/// source. Must run on the main thread.
pub(crate) fn pump_input_source_runloop() {
    debug_assert!(
        // SAFETY: `pthread_main_np` is always safe to call.
        unsafe { libc::pthread_main_np() } != 0,
        "pump_input_source_runloop must run on the main thread"
    );
    // SAFETY: `CFRunLoopRunInMode` is thread-safe; a 0-second call drains the ready sources and
    // returns immediately (no blocking). `CF_RUN_LOOP_DEFAULT_MODE` is a framework-owned constant.
    unsafe {
        let _ = cf_run_loop_run_in_mode(CF_RUN_LOOP_DEFAULT_MODE, 0.0, 0);
    }
}

#[derive(Debug)]
struct RetainedInputSource(NonNull<TisInputSource>);

impl RetainedInputSource {
    /// Takes ownership of a retained reference returned by a TIS `Copy` function.
    unsafe fn from_copy(raw: TisInputSourceRef) -> Option<Self> {
        NonNull::new(raw as *mut TisInputSource).map(Self)
    }

    fn select(&self) -> OsStatus {
        // SAFETY: this wrapper keeps the retained input source alive for the call.
        unsafe { tis_select_input_source(self.0.as_ptr()) }
    }

    fn has_same_id(&self, other: &Self) -> bool {
        // SAFETY: TIS property values stay valid while their input sources are alive;
        // both wrappers outlive this comparison.
        unsafe {
            let left = tis_get_input_source_property(self.0.as_ptr(), TIS_PROPERTY_INPUT_SOURCE_ID);
            let right =
                tis_get_input_source_property(other.0.as_ptr(), TIS_PROPERTY_INPUT_SOURCE_ID);
            !left.is_null() && !right.is_null() && cf_equal(left, right) != 0
        }
    }
}

impl Drop for RetainedInputSource {
    fn drop(&mut self) {
        // SAFETY: `from_copy` gives this wrapper ownership of one retain.
        unsafe { cf_release(self.0.as_ptr().cast()) }
    }
}

#[derive(Debug)]
pub(crate) struct InputSourceRestore {
    previous: RetainedInputSource,
}

impl Drop for InputSourceRestore {
    fn drop(&mut self) {
        let status = self.previous.select();
        if status != 0 {
            tracing::debug!(
                status,
                "failed to restore host input source after prefix mode"
            );
        }
    }
}

pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    // SAFETY: both Carbon `Copy` functions transfer one retain to the caller.
    let current =
        unsafe { RetainedInputSource::from_copy(tis_copy_current_keyboard_input_source())? };
    let ascii = unsafe {
        RetainedInputSource::from_copy(
            tis_copy_current_ascii_capable_keyboard_layout_input_source(),
        )?
    };

    if current.has_same_id(&ascii) {
        return None;
    }

    let status = ascii.select();
    if status != 0 {
        tracing::debug!(status, "failed to switch host input source for prefix mode");
        return None;
    }

    Some(InputSourceRestore { previous: current })
}
