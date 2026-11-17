use core::ffi::c_int;
use core::marker::PhantomData;

#[cfg(all(not(windows), not(miri)))]
use core::hint::spin_loop;
#[cfg(all(not(windows), not(miri)))]
use core::mem::MaybeUninit;
#[cfg(all(not(windows), not(miri)))]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(all(not(windows), not(miri)))]
static POLICY_LOCK: AtomicBool = AtomicBool::new(false);
#[cfg(all(not(windows), not(miri)))]
static POLICY_OWNER: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(not(windows), not(miri)))]
struct PolicyState {
    action: libc::sigaction,
    mask: libc::sigset_t,
}

#[cfg(all(not(windows), not(miri)))]
fn sigpipe_set() -> Result<libc::sigset_t, ()> {
    let mut set = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: set points to writable storage for a sigset_t.
    if unsafe { libc::sigemptyset(set.as_mut_ptr()) } != 0 {
        return Err(());
    }
    // SAFETY: sigemptyset initialized set above.
    let mut set = unsafe { set.assume_init() };
    // SAFETY: set is initialized and SIGPIPE is valid on supported Unix targets.
    if unsafe { libc::sigaddset(&mut set, libc::SIGPIPE) } != 0 {
        return Err(());
    }
    Ok(set)
}

#[cfg(all(not(windows), not(miri)))]
fn default_sigpipe_action() -> Result<libc::sigaction, ()> {
    // SAFETY: zero is valid for the C sigaction fields, including its mask,
    // default handler, flags, and platform restorer field.
    let mut action = unsafe { MaybeUninit::<libc::sigaction>::zeroed().assume_init() };
    action.sa_sigaction = libc::SIG_DFL;
    // SAFETY: action.sa_mask is writable storage for a sigset_t.
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
        return Err(());
    }
    Ok(action)
}

#[cfg(all(not(windows), not(miri)))]
fn enter_generated_policy() -> Result<PolicyState, ()> {
    let set = sigpipe_set()?;
    let mut previous_mask = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: set is initialized and previous_mask is a valid out-pointer.
    // Blocking SIGPIPE during the transition prevents pending-signal delivery
    // while the action and thread mask are between states.
    if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, previous_mask.as_mut_ptr()) } != 0 {
        return Err(());
    }
    // SAFETY: pthread_sigmask initialized previous_mask above.
    let previous_mask = unsafe { previous_mask.assume_init() };

    let result = (|| {
        let mut previous_action = MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: a null new action queries the current action without changing it.
        if unsafe {
            libc::sigaction(
                libc::SIGPIPE,
                core::ptr::null(),
                previous_action.as_mut_ptr(),
            )
        } != 0
        {
            return Err(());
        }
        // SAFETY: sigaction initialized previous_action above.
        let previous_action = unsafe { previous_action.assume_init() };
        let default_action = default_sigpipe_action()?;
        // SAFETY: default_action is initialized and oldact is not needed.
        if unsafe { libc::sigaction(libc::SIGPIPE, &default_action, core::ptr::null_mut()) } != 0 {
            return Err(());
        }
        // SAFETY: set is initialized and oldset is not needed. Generated code
        // always runs with SIGPIPE unblocked, even if its launcher blocked it.
        if unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, core::ptr::null_mut()) } != 0 {
            // SAFETY: previous_action is initialized; best-effort rollback.
            unsafe { libc::sigaction(libc::SIGPIPE, &previous_action, core::ptr::null_mut()) };
            return Err(());
        }
        Ok(PolicyState {
            action: previous_action,
            mask: previous_mask,
        })
    })();

    if result.is_err() {
        // SAFETY: previous_mask is initialized; restore the entry mask after
        // any failed transition so the host does not retain a partial setup.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous_mask, core::ptr::null_mut()) };
    }
    result
}

pub(crate) fn install_generated_process_policy() -> Result<(), ()> {
    #[cfg(all(not(windows), not(miri)))]
    {
        enter_generated_policy().map(|_| ())
    }
    #[cfg(any(windows, miri))]
    {
        Ok(())
    }
}

/// Owns the generated-program signal policy for one hosted execution. The
/// current CLI excludes concurrent host execution that needs another SIGPIPE
/// policy. Same-thread reentry is rejected instead of waiting on itself.
pub struct GeneratedProcessPolicyGuard {
    #[cfg(all(not(windows), not(miri)))]
    previous: PolicyState,
    _not_send: PhantomData<*mut ()>,
}

impl GeneratedProcessPolicyGuard {
    pub fn enter() -> Option<Self> {
        #[cfg(all(not(windows), not(miri)))]
        {
            // SAFETY: pthread_self returns the calling thread's nonzero ID on
            // supported Unix targets.
            let current_thread = unsafe { libc::pthread_self() } as usize;
            if POLICY_OWNER.load(Ordering::Acquire) == current_thread {
                return None;
            }
            while POLICY_LOCK
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                spin_loop();
            }
            POLICY_OWNER.store(current_thread, Ordering::Release);
            match enter_generated_policy() {
                Ok(previous) => Some(Self {
                    previous,
                    _not_send: PhantomData,
                }),
                Err(()) => {
                    POLICY_OWNER.store(0, Ordering::Release);
                    POLICY_LOCK.store(false, Ordering::Release);
                    None
                }
            }
        }
        #[cfg(any(windows, miri))]
        {
            Some(Self {
                _not_send: PhantomData,
            })
        }
    }
}

impl Drop for GeneratedProcessPolicyGuard {
    fn drop(&mut self) {
        #[cfg(all(not(windows), not(miri)))]
        {
            if let Ok(set) = sigpipe_set() {
                // SAFETY: set is initialized and oldset is not needed. Block
                // pending generated signals before restoring host state.
                unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, core::ptr::null_mut()) };
            }
            // SAFETY: previous contains the complete action saved on entry.
            unsafe { libc::sigaction(libc::SIGPIPE, &self.previous.action, core::ptr::null_mut()) };
            // SAFETY: previous.mask is the thread mask saved on entry.
            unsafe {
                libc::pthread_sigmask(
                    libc::SIG_SETMASK,
                    &self.previous.mask,
                    core::ptr::null_mut(),
                )
            };
            POLICY_OWNER.store(0, Ordering::Release);
            POLICY_LOCK.store(false, Ordering::Release);
        }
    }
}

#[cfg(all(not(windows), not(miri)))]
pub(crate) fn handle_write_error(error: c_int) {
    if error == libc::EPIPE {
        // SAFETY: generated entry installed SIG_DFL and unblocked SIGPIPE.
        // Raising it delivers termination before a JIT guard can restore the
        // host's previous action and mask on normal return.
        unsafe { libc::raise(libc::SIGPIPE) };
    }
}

#[cfg(miri)]
pub(crate) fn handle_write_error(_error: c_int) {}

#[cfg(windows)]
pub(crate) fn handle_write_error(error: c_int) {
    const BROKEN_PIPE_EXIT: c_int = 141;

    unsafe extern "C" {
        fn exit(code: c_int) -> !;
    }

    if error == libc::EPIPE {
        // SAFETY: exit never returns.
        unsafe { exit(BROKEN_PIPE_EXIT) }
    }
}
