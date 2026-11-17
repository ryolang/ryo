use core::ffi::c_int;

#[cfg(all(not(windows), not(miri)))]
type SignalHandler = usize;

#[cfg(all(not(windows), not(miri)))]
const SIGPIPE: c_int = 13;
#[cfg(all(not(windows), not(miri)))]
const SIG_DFL: SignalHandler = 0;
#[cfg(all(not(windows), not(miri)))]
const SIG_ERR: SignalHandler = usize::MAX;

#[cfg(all(not(windows), not(miri)))]
unsafe extern "C" {
    fn signal(signal: c_int, handler: SignalHandler) -> SignalHandler;
}

#[cfg(all(not(windows), not(miri)))]
fn install_default_sigpipe() -> Result<SignalHandler, ()> {
    // SAFETY: SIGPIPE is valid on supported Unix targets and SIG_DFL is the
    // POSIX default-disposition sentinel. The returned value is retained by
    // the JIT guard and passed back unchanged on normal return.
    let previous = unsafe { signal(SIGPIPE, SIG_DFL) };
    if previous == SIG_ERR {
        Err(())
    } else {
        Ok(previous)
    }
}

pub(crate) fn install_generated_process_policy() -> Result<(), ()> {
    #[cfg(all(not(windows), not(miri)))]
    {
        install_default_sigpipe().map(|_| ())
    }
    #[cfg(any(windows, miri))]
    {
        Ok(())
    }
}

pub struct GeneratedProcessPolicyGuard {
    #[cfg(all(not(windows), not(miri)))]
    previous: SignalHandler,
}

impl GeneratedProcessPolicyGuard {
    pub fn enter() -> Result<Self, ()> {
        #[cfg(all(not(windows), not(miri)))]
        {
            install_default_sigpipe().map(|previous| Self { previous })
        }
        #[cfg(any(windows, miri))]
        {
            Ok(Self {})
        }
    }
}

impl Drop for GeneratedProcessPolicyGuard {
    fn drop(&mut self) {
        #[cfg(all(not(windows), not(miri)))]
        {
            // SAFETY: `previous` came from signal's successful return in
            // enter and has not been transformed.
            unsafe { signal(SIGPIPE, self.previous) };
        }
    }
}

#[cfg(all(not(windows), not(miri)))]
pub(crate) fn handle_write_error(error: c_int) {
    const EPIPE: c_int = 32;

    unsafe extern "C" {
        fn raise(signal: c_int) -> c_int;
    }

    if error == EPIPE {
        // SAFETY: generated entry installed SIG_DFL for SIGPIPE. Raising it
        // here delivers the required termination before a JIT guard can
        // restore the host's previous disposition on normal return.
        unsafe { raise(SIGPIPE) };
    }
}

#[cfg(miri)]
pub(crate) fn handle_write_error(_error: c_int) {}

#[cfg(windows)]
pub(crate) fn handle_write_error(error: c_int) {
    const EPIPE: c_int = 32;
    const BROKEN_PIPE_EXIT: c_int = 141;

    unsafe extern "C" {
        fn exit(code: c_int) -> !;
    }

    if error == EPIPE {
        // SAFETY: exit never returns.
        unsafe { exit(BROKEN_PIPE_EXIT) }
    }
}
