// The staticlib archive linked by `zig cc` is `#![no_std]` and allocates
// through the C heap (malloc/free/realloc), so it bundles no precompiled
// std/alloc objects — those carry `_Unwind_*`/`rust_eh_personality`
// references that nothing satisfies at the final link. The rlib linked
// into the std JIT host and the test harness use the same code paths
// against the host libc.
#![cfg_attr(feature = "staticlib", no_std)]

// Test builds link std through the harness; the gate keeps `std::`
// paths available in test code if needed.
#[cfg(test)]
extern crate std;

use core::ffi::{c_int, c_void};

const STDOUT_FD: c_int = 1;
const STDERR_FD: c_int = 2;

#[cfg(not(windows))]
unsafe extern "C" {
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
}

#[cfg(windows)]
unsafe extern "C" {
    fn _write(fd: c_int, buf: *const c_void, count: u32) -> c_int;
    fn _setmode(fd: c_int, mode: c_int) -> c_int;
}

/// `_O_BINARY` — no `\n` → `\r\n` translation on write.
#[cfg(windows)]
const O_BINARY: c_int = 0x8000;

// MSVC's CRT defines `_fltused`; float code in core/ryu references it.
// Rustc-linked binaries get it from the CRT, but the no_std archive is
// linked by `zig cc`, which provides no definition — supply it here.
#[cfg(all(windows, feature = "staticlib"))]
#[unsafe(no_mangle)]
#[used]
pub static _fltused: c_int = 0;

unsafe extern "C" {
    fn exit(code: c_int) -> !;
    fn abort() -> !;
}

unsafe extern "C" {
    #[link_name = "malloc"]
    fn c_malloc(size: usize) -> *mut c_void;
    #[link_name = "free"]
    fn c_free(ptr: *mut c_void);
    #[link_name = "realloc"]
    fn c_realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
}

/// Thin wrapper over the C `write`/`_write` for one fd.
/// Returns the byte count written, or <= 0 on error.
fn os_write(fd: c_int, ptr: *const u8, len: usize) -> isize {
    #[cfg(not(windows))]
    // SAFETY: caller guarantees ptr is readable for len bytes; the call
    // does not retain the buffer.
    unsafe {
        write(fd, ptr.cast::<c_void>(), len)
    }
    #[cfg(windows)]
    // SAFETY: same. `_write` takes a u32 count; clamp (print/panic
    // payloads are strings, far below 4 GiB in practice). `_setmode`
    // only flips a per-fd CRT flag; repeated calls are idempotent.
    // Binary mode is required: the CRT's default text mode translates
    // \n → \r\n, and print/panic must emit the exact bytes given.
    unsafe {
        _setmode(fd, O_BINARY);
        _write(fd, ptr.cast::<c_void>(), len.min(u32::MAX as usize) as u32) as isize
    }
}

/// Write all `len` bytes to `fd`, retrying short writes. Gives
/// up silently on hard errors (return <= 0): stdout/stderr output is
/// best-effort and there is no error channel to report through.
fn write_all(fd: c_int, mut ptr: *const u8, mut len: usize) {
    while len > 0 {
        let n = os_write(fd, ptr, len);
        if n <= 0 {
            return;
        }
        // SAFETY: os_write reported n bytes consumed, so advancing by n
        // stays within (or at most one past) the caller's buffer.
        ptr = unsafe { ptr.add(n as usize) };
        len -= n as usize;
    }
}

/// Runtime backing for the `print` builtin: write the viewed
/// bytes to stdout. No added newline, no formatting — print policy is
/// a spec-level decision, not a runtime one.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (or be null/dangling when
/// `len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_print(ptr: *const u8, len: u64) {
    if len == 0 {
        return;
    }
    if ptr.is_null() {
        null_abort();
    }
    write_all(STDOUT_FD, ptr, len as usize);
}

/// Runtime backing for `__ryo_panic` (panic/assert): write the
/// sema-formatted message to stderr and exit 101.
///
/// # Safety
/// `ptr` must point to `len` readable bytes. Never returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_panic(ptr: *const u8, len: u64) -> ! {
    if len > 0 {
        if ptr.is_null() {
            null_abort();
        }
        write_all(STDERR_FD, ptr, len as usize);
    }
    // SAFETY: exit never returns.
    unsafe { exit(101) }
}

#[cfg(feature = "staticlib")]
#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    // The archive is linked by `zig cc` without Rust std; panics from
    // bounds/overflow checks in runtime code land here. The workspace
    // builds with panic = "abort", so there is no unwinding to support.
    // SAFETY: abort never returns.
    unsafe { abort() }
}

/// Precompiled `core` objects carry eh-frame references to
/// `rust_eh_personality` even though the workspace builds with
/// panic = "abort" and nothing ever unwinds. The symbol only needs to
/// resolve at the final zig-cc link; it is never called.
#[cfg(feature = "staticlib")]
#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}

#[repr(C)]
pub struct RyoStrFat {
    pub ptr: *mut u8,
    pub len: u64,
    pub cap: u64,
}

/// Inline capacity of the small-string optimization (SSO): strings of
/// at most this many bytes live directly inside the 24-byte slot.
/// 23 >= 20, so every `int_to_str`/`bool_to_str` output is inline.
pub(crate) const INLINE_CAP: usize = 23;

/// Cap-word tag: the top byte (byte 23, little-endian) discriminates.
/// `0x80 | len` marks an inline string; a top byte of `0x00` is a heap
/// cap (caps stay below 2^56 by construction) or the all-zero static
/// `.rodata` sentinel.
pub(crate) fn inline_tag(len: u64) -> u64 {
    debug_assert!(len <= INLINE_CAP as u64);
    (0x80 | len) << 56
}

pub(crate) fn is_inline(cap: u64) -> bool {
    (cap >> 56) & 0x80 != 0
}

pub(crate) fn inline_len(cap: u64) -> u64 {
    debug_assert!(is_inline(cap));
    (cap >> 56) & 0x7f
}

/// Write ONLY the tag byte (byte 23) of a slot, marking it inline with
/// the given length. The low 7 bytes of the cap word (offsets 16–22)
/// are inline DATA for strings of length 17–23 — a full-word
/// `(*out).cap = inline_tag(len)` store would zero them and corrupt the
/// string. Every inline retag goes through this helper.
///
/// # Safety
/// `out` points to a valid 24-byte `RyoStrFat` whose inline data bytes
/// (offsets 0..len) are already initialized.
pub(crate) unsafe fn write_inline_tag(out: *mut RyoStrFat, len: u64) {
    debug_assert!(len <= INLINE_CAP as u64);
    // SAFETY: caller contract — out is valid for 24 bytes; we touch only
    // byte 23, leaving data bytes 0..=22 intact.
    unsafe {
        (out as *mut u8)
            .add(23)
            .write((inline_tag(len) >> 56) as u8)
    };
}

/// Heap capacity policy for producers that want push-ready headroom:
/// next power of two above `min`, floor 16. Matches `__ryo_str_push`'s
/// doubling so a produced buffer grows smoothly.
pub(crate) fn growth_cap(min: u64) -> u64 {
    // checked_next_power_of_two returns exactly 2^56 for min in
    // [2^55, 2^56), which would set the tag byte — cap the input one
    // power lower so caps stay below 2^56 by construction.
    debug_assert!(min < (1 << 55), "cap must keep the tag byte clear");
    min.checked_next_power_of_two()
        .unwrap_or_else(|| overflow_abort())
        .max(16)
}

/// Write `bytes` into `out` as a tagged slot: inline when it fits,
/// else a heap allocation with growth headroom. Producer ABI for every
/// slot-out runtime function.
///
/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat` (24 bytes).
unsafe fn write_str_slot(out: *mut RyoStrFat, bytes: &[u8]) {
    let len = bytes.len();
    if len <= INLINE_CAP {
        // SAFETY: out is valid for 24 bytes; len <= 23 fits the inline
        // data region (offsets 0..=22).
        unsafe {
            if len > 0 {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, len);
            }
            // SAFETY: out is valid and its inline data bytes 0..len are
            // initialized by the copy above. Byte-23-only tag write: a
            // full cap-word store would zero data bytes 16..len when
            // len > 16.
            write_inline_tag(out, len as u64);
        }
    } else {
        let cap = growth_cap(len as u64);
        let buf = ryo_str_alloc(cap);
        // SAFETY: buf is freshly allocated for cap >= len bytes; the
        // source slice is readable for len bytes; regions do not overlap.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, len);
            *out = RyoStrFat {
                ptr: buf,
                len: len as u64,
                cap,
            };
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ryo_str_alloc(cap: u64) -> *mut u8 {
    if cap == 0 {
        return core::ptr::null_mut();
    }
    let size: usize = cap.try_into().unwrap_or_else(|_| overflow_abort());
    // SAFETY: malloc is called with a nonzero size.
    let ptr = unsafe { c_malloc(size) as *mut u8 };
    if ptr.is_null() {
        oom_abort();
    }
    ptr
}

/// # Safety
/// `ptr` must have been returned by `ryo_str_alloc` or `ryo_str_realloc`,
/// or be null. `cap` is the tagged cap word: an inline (`0x80`-tagged)
/// cap and `cap == 0` (the static `.rodata` sentinel) are both no-ops.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_str_free(ptr: *mut u8, cap: u64) {
    // Tag check FIRST: for an inline string the ptr word is byte data,
    // never a heap pointer — nothing to free. Then the static sentinel.
    if is_inline(cap) || ptr.is_null() || cap == 0 {
        return;
    }
    // SAFETY: caller contract — ptr came from ryo_str_alloc/realloc.
    unsafe { c_free(ptr as *mut c_void) };
}

/// # Safety
/// `ptr` must have been returned by `ryo_str_alloc` or `ryo_str_realloc`
/// with the given `old_cap`, or be null. `old_cap` must be a heap cap
/// (tag byte clear): this function is not tag-aware and must never be
/// handed an inline slot's tagged cap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_str_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
    if ptr.is_null() || old_cap == 0 {
        return ryo_str_alloc(new_cap);
    }
    if new_cap == 0 {
        // SAFETY: ptr/old_cap came from a prior alloc per our # Safety doc.
        unsafe { ryo_str_free(ptr, old_cap) };
        return core::ptr::null_mut();
    }
    let new_size: usize = new_cap.try_into().unwrap_or_else(|_| overflow_abort());
    // SAFETY: ptr came from a prior alloc per our # Safety doc; new_size > 0 checked above.
    let new_ptr = unsafe { c_realloc(ptr as *mut c_void, new_size) as *mut u8 };
    if new_ptr.is_null() {
        oom_abort();
    }
    new_ptr
}

fn oom_abort() -> ! {
    let msg = b"ryo: out of memory\n";
    write_all(STDERR_FD, msg.as_ptr(), msg.len());
    // SAFETY: abort never returns.
    unsafe { abort() }
}

#[cold]
fn overflow_abort() -> ! {
    let msg = b"ryo: capacity overflow\n";
    write_all(STDERR_FD, msg.as_ptr(), msg.len());
    // SAFETY: abort never returns.
    unsafe { abort() }
}

#[cold]
fn null_abort() -> ! {
    let msg = b"ryo: null pointer passed to runtime\n";
    write_all(STDERR_FD, msg.as_ptr(), msg.len());
    // SAFETY: abort never returns.
    unsafe { abort() }
}

/// Materialize an owned `str` copy from a `strview` (M8.4.1.2), written
/// as a tagged slot: inline when `len <= 23`, else a fresh heap buffer
/// with growth headroom. `len == 0` yields the inline-empty slot.
///
/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`. `ptr` must point
/// to `len` readable bytes — or be null/dangling when `len == 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_str_from_view(out: *mut RyoStrFat, ptr: *const u8, len: u64) {
    if len == 0 {
        // SAFETY: out is a valid out-slot.
        unsafe { write_str_slot(out, b"") };
        return;
    }
    let n: usize = len.try_into().unwrap_or_else(|_| overflow_abort());
    if ptr.is_null() {
        null_abort();
    }
    // SAFETY: caller contract — ptr/len describe a readable byte range.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, n) };
    // SAFETY: out is a valid out-slot; `bytes` holds `len` initialized bytes.
    unsafe { write_str_slot(out, bytes) };
}

fn slice_fail(msg: &str) -> ! {
    // Raw message + newline, exit 101 — same contract as `ryo_panic`.
    write_all(STDERR_FD, msg.as_ptr(), msg.len());
    write_all(STDERR_FD, b"\n".as_ptr(), 1);
    // SAFETY: exit never returns.
    unsafe { exit(101) }
}

/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`. `l_ptr`/`r_ptr`
/// point to `l_len`/`r_len` readable bytes (or are null/dangling when
/// the len is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_str_concat(
    out: *mut RyoStrFat,
    l_ptr: *const u8,
    l_len: u64,
    r_ptr: *const u8,
    r_len: u64,
) {
    let total = match l_len.checked_add(r_len) {
        Some(t) => t,
        None => overflow_abort(),
    };
    if total == 0 {
        // SAFETY: out is a valid out-slot.
        unsafe {
            *out = RyoStrFat {
                ptr: core::ptr::null_mut(),
                len: 0,
                cap: 0,
            }
        };
        return;
    }
    let l_sz: usize = l_len.try_into().unwrap_or_else(|_| overflow_abort());
    let r_sz: usize = r_len.try_into().unwrap_or_else(|_| overflow_abort());
    if total as usize <= INLINE_CAP {
        // Build inline: write both halves into the slot's data region.
        // SAFETY: out is valid for 24 bytes; total <= 23 fits inline;
        // inputs are readable per the caller contract.
        unsafe {
            let dst = out as *mut u8;
            if l_sz > 0 {
                debug_assert!(!l_ptr.is_null());
                core::ptr::copy_nonoverlapping(l_ptr, dst, l_sz);
            }
            if r_sz > 0 {
                debug_assert!(!r_ptr.is_null());
                core::ptr::copy_nonoverlapping(r_ptr, dst.add(l_sz), r_sz);
            }
            write_inline_tag(out, total);
        }
        return;
    }
    let cap = growth_cap(total);
    let ptr = ryo_str_alloc(cap);
    // SAFETY: ptr is freshly allocated for cap >= total bytes; inputs
    // are readable per the caller contract; regions do not overlap.
    unsafe {
        if l_sz > 0 {
            debug_assert!(!l_ptr.is_null());
            core::ptr::copy_nonoverlapping(l_ptr, ptr, l_sz);
        }
        if r_sz > 0 {
            debug_assert!(!r_ptr.is_null());
            core::ptr::copy_nonoverlapping(r_ptr, ptr.add(l_sz), r_sz);
        }
        *out = RyoStrFat {
            ptr,
            len: total,
            cap,
        };
    }
}

/// Append `suffix` to the str fat-pointer at `s_ptr`, reallocating if the
/// existing capacity cannot hold the result, and write the new
/// (ptr, len, cap) back through `s_ptr`. This is the runtime backing for
/// the M8.3 `str_push(s: inout str, suffix: str)` builtin.
///
/// # Safety
/// `s_ptr` points to a valid `RyoStrFat` owned by the caller;
/// `suffix_ptr`/`suffix_len` describe a valid readable byte range
/// (which may be empty / null when `suffix_len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ryo_str_push(
    s_ptr: *mut RyoStrFat,
    suffix_ptr: *const u8,
    suffix_len: u64,
) {
    // SAFETY: s_ptr is a valid RyoStrFat per the ABI contract; the suffix
    // range is valid for reading and does not overlap the destination
    // buffer (the caller owns disjoint storage).
    unsafe {
        let cur_ptr = (*s_ptr).ptr;
        let cur_len = (*s_ptr).len;
        let cur_cap = (*s_ptr).cap;
        let add: u64 = suffix_len;

        if is_inline(cur_cap) {
            // Inline source: bytes live in the slot itself. The slot's
            // `ptr`/`len` words ARE byte data here — the real length
            // lives only in the cap-word tag, so new_len must be
            // computed from inline_len, never from the len word.
            let ilen = inline_len(cur_cap);
            let new_len = match ilen.checked_add(add) {
                Some(l) => l,
                None => overflow_abort(),
            };
            if new_len <= INLINE_CAP as u64 {
                // SAFETY: slot data region holds ilen bytes; appending
                // add bytes stays within INLINE_CAP; suffix readable.
                // Disjointness holds even for push(s, s[0:2]): the suffix
                // range is within slot bytes [0, ilen) while the
                // destination is [ilen, ilen + add).
                if add > 0 {
                    debug_assert!(!suffix_ptr.is_null());
                    core::ptr::copy_nonoverlapping(
                        suffix_ptr,
                        (s_ptr as *mut u8).add(ilen as usize),
                        add as usize,
                    );
                }
                // SAFETY: slot data region holds new_len initialized
                // bytes; retag touches byte 23 only (a full-word cap
                // store would zero data bytes 16–22).
                write_inline_tag(s_ptr, new_len);
                return;
            }
            // Promote: copy inline bytes out BEFORE overwriting the slot,
            // then fall into a heap buffer with growth headroom.
            let mut tmp = [0u8; INLINE_CAP];
            // SAFETY: slot data region holds ilen <= INLINE_CAP bytes;
            // tmp is a full INLINE_CAP stack buffer; regions disjoint.
            core::ptr::copy_nonoverlapping(s_ptr as *const u8, tmp.as_mut_ptr(), ilen as usize);
            let new_cap = growth_cap(new_len);
            let nb = ryo_str_alloc(new_cap);
            // SAFETY: nb is freshly allocated for new_cap >= new_len
            // bytes; tmp holds ilen bytes; regions disjoint.
            core::ptr::copy_nonoverlapping(tmp.as_ptr(), nb, ilen as usize);
            if add > 0 {
                debug_assert!(!suffix_ptr.is_null());
                // SAFETY: suffix readable for add bytes; nb + ilen has
                // add bytes of room (new_cap >= new_len = ilen + add);
                // regions disjoint per the caller contract.
                core::ptr::copy_nonoverlapping(suffix_ptr, nb.add(ilen as usize), add as usize);
            }
            *s_ptr = RyoStrFat {
                ptr: nb,
                len: new_len,
                cap: new_cap,
            };
            return;
        }

        // Non-inline sources (heap or the cap==0 static sentinel): the
        // len word is a real length.
        let new_len = match cur_len.checked_add(add) {
            Some(l) => l,
            None => overflow_abort(),
        };
        if cur_cap == 0 && new_len <= INLINE_CAP as u64 {
            // Static (.rodata) source, short result: copy off rodata
            // into the slot as inline — no heap allocation at all.
            // Read the static bytes into a temp BEFORE overwriting.
            let mut tmp = [0u8; INLINE_CAP];
            if cur_len > 0 {
                debug_assert!(!cur_ptr.is_null());
                core::ptr::copy_nonoverlapping(cur_ptr, tmp.as_mut_ptr(), cur_len as usize);
            }
            // SAFETY: tmp holds the old bytes; slot data region fits
            // new_len <= INLINE_CAP bytes; suffix readable.
            core::ptr::copy_nonoverlapping(tmp.as_ptr(), s_ptr as *mut u8, cur_len as usize);
            if add > 0 {
                debug_assert!(!suffix_ptr.is_null());
                core::ptr::copy_nonoverlapping(
                    suffix_ptr,
                    (s_ptr as *mut u8).add(cur_len as usize),
                    add as usize,
                );
            }
            // SAFETY: slot data region holds new_len initialized bytes;
            // retag touches byte 23 only.
            write_inline_tag(s_ptr, new_len);
            return;
        }

        // Reuse the current buffer when it already fits; otherwise grow.
        // Capacity policy: double the old capacity (or fit exactly when
        // the old buffer was empty) — a tighter ARC/CoW policy is a
        // post-M11 concern.
        let (buf, cap) = if new_len <= cur_cap {
            (cur_ptr, cur_cap)
        } else {
            let new_cap = if cur_cap == 0 {
                new_len
            } else {
                cur_cap.saturating_mul(2).max(new_len)
            };
            if cur_cap == 0 {
                // Static (.rodata sentinel) source: cap==0 means the ptr is
                // NOT heap-owned, so `ryo_str_realloc` would allocate fresh
                // WITHOUT copying. Allocate here and copy the existing
                // `cur_len` bytes explicitly.
                let nb = ryo_str_alloc(new_cap);
                if cur_len > 0 {
                    let n: usize = cur_len.try_into().unwrap_or_else(|_| overflow_abort());
                    debug_assert!(!cur_ptr.is_null());
                    core::ptr::copy_nonoverlapping(cur_ptr, nb, n);
                }
                (nb, new_cap)
            } else {
                // Heap-owned: realloc copies the old contents and frees
                // the old buffer.
                let nb = ryo_str_realloc(cur_ptr, cur_cap, new_cap);
                (nb, new_cap)
            }
        };

        if add > 0 {
            let dst_off: usize = cur_len.try_into().unwrap_or_else(|_| overflow_abort());
            let n: usize = add.try_into().unwrap_or_else(|_| overflow_abort());
            debug_assert!(!suffix_ptr.is_null());
            core::ptr::copy_nonoverlapping(suffix_ptr, buf.add(dst_off), n);
        }
        (*s_ptr).ptr = buf;
        (*s_ptr).len = new_len;
        (*s_ptr).cap = cap;
    }
}

/// Promote an inline (SSO) string to a heap buffer in place, writing
/// the heap triple back through `s_ptr`. No-op for heap and static
/// (`cap == 0`) strings. Called by codegen before any view-creating op
/// (slice, view conversion) so views always point at memory that never
/// moves — inline bytes live in the slot and would dangle.
///
/// # Safety
/// `s_ptr` points to a valid tagged `RyoStrFat` owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ryo_str_ensure_heap(s_ptr: *mut RyoStrFat) {
    // SAFETY: s_ptr is a valid tagged slot per the ABI contract.
    unsafe {
        let cap = (*s_ptr).cap;
        if !is_inline(cap) {
            return;
        }
        let len = inline_len(cap) as usize;
        // Copy the inline bytes out BEFORE overwriting the slot.
        let mut tmp = [0u8; INLINE_CAP];
        core::ptr::copy_nonoverlapping(s_ptr as *const u8, tmp.as_mut_ptr(), len);
        let new_cap = growth_cap(len as u64);
        let buf = ryo_str_alloc(new_cap);
        // SAFETY: buf is freshly allocated for new_cap >= len bytes;
        // tmp holds the inline bytes; regions do not overlap.
        core::ptr::copy_nonoverlapping(tmp.as_ptr(), buf, len);
        *s_ptr = RyoStrFat {
            ptr: buf,
            len: len as u64,
            cap: new_cap,
        };
    }
}

/// Bytes twin of `__ryo_str_ensure_heap` — promotion is
/// representation-only, no UTF-8 concerns.
///
/// # Safety
/// `s_ptr` points to a valid tagged `RyoStrFat` owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ryo_bytes_ensure_heap(s_ptr: *mut RyoStrFat) {
    // SAFETY: forwarded contract.
    unsafe { __ryo_str_ensure_heap(s_ptr) };
}

/// # Safety
/// `a_ptr` must point to `a_len` readable bytes (or be null/dangling if a_len==0).
/// Same for `b_ptr`/`b_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_str_eq(
    a_ptr: *const u8,
    a_len: u64,
    b_ptr: *const u8,
    b_len: u64,
) -> u8 {
    if a_len != b_len {
        return 0;
    }
    if a_len == 0 {
        return 1;
    }
    // SAFETY: caller contract — a_ptr/a_len and b_ptr/b_len describe valid byte ranges.
    let a_slice = unsafe { core::slice::from_raw_parts(a_ptr, a_len as usize) };
    let b_slice = unsafe { core::slice::from_raw_parts(b_ptr, b_len as usize) };
    if a_slice == b_slice { 1 } else { 0 }
}

/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_int_to_str(out: *mut RyoStrFat, value: i64) {
    let mut buf = [0u8; 32];
    let negative = value < 0;
    // Work with unsigned magnitude to handle i64::MIN correctly
    // (i64::MIN.wrapping_neg() overflows back to i64::MIN).
    let mut n: u64 = if negative {
        (value as u64).wrapping_neg()
    } else {
        value as u64
    };
    let mut pos = buf.len();
    if n == 0 {
        pos -= 1;
        buf[pos] = b'0';
    } else {
        while n > 0 {
            pos -= 1;
            buf[pos] = b'0' + (n % 10) as u8;
            n /= 10;
        }
    }
    if negative {
        pos -= 1;
        buf[pos] = b'-';
    }
    // SAFETY: out is a valid out-slot; buf[pos..] holds the formatted
    // digits (at most 20 bytes, always inline).
    unsafe { write_str_slot(out, &buf[pos..]) };
}

/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_float_to_str(out: *mut RyoStrFat, value: f64) {
    if value.is_nan() {
        // SAFETY: out is a valid out-slot.
        unsafe { write_str_slot(out, b"nan") };
        return;
    }
    if value.is_infinite() {
        // SAFETY: out is a valid out-slot.
        unsafe { write_str_slot(out, if value < 0.0 { b"-inf" } else { b"inf" }) };
        return;
    }

    let mut buf = ryu::Buffer::new();
    // SAFETY: out is a valid out-slot; the ryu buffer holds the
    // formatted bytes.
    unsafe { write_str_slot(out, buf.format(value).as_bytes()) };
}

/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_bool_to_str(out: *mut RyoStrFat, value: u8) {
    // SAFETY: out is a valid out-slot.
    unsafe { write_str_slot(out, if value != 0 { b"true" } else { b"false" }) };
}

// ---------- bytes (M8.4.2) ----------
//
// Owned `bytes` buffers mirror the `str` ABI exactly: concat, from_view,
// and the conversions write tagged slots; `__ryo_bytes_push` manages
// growth through the same 24-byte slot ABI.
// No UTF-8 invariants anywhere in this family.

#[unsafe(no_mangle)]
pub extern "C" fn ryo_bytes_alloc(cap: u64) -> *mut u8 {
    ryo_str_alloc(cap)
}

/// # Safety
/// `ptr` must have been returned by `ryo_bytes_alloc` /
/// `ryo_bytes_realloc` with the given `cap`, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_bytes_free(ptr: *mut u8, cap: u64) {
    // SAFETY: caller contract forwarded to `ryo_str_free`.
    unsafe { ryo_str_free(ptr, cap) };
}

/// # Safety
/// `ptr` must have been returned by `ryo_bytes_alloc` /
/// `ryo_bytes_realloc` with the given `old_cap`, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_bytes_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
    // SAFETY: caller contract forwarded to `ryo_str_realloc`.
    unsafe { ryo_str_realloc(ptr, old_cap, new_cap) }
}

/// Materialize an owned `bytes` copy from a `bytesview` (M8.4.2),
/// written as a tagged slot: inline when `len <= 23`, else a fresh heap
/// buffer with growth headroom. `len == 0` yields the inline-empty slot.
///
/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`. `ptr` must point
/// to `len` readable bytes — or be null/dangling when `len == 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_bytes_from_view(out: *mut RyoStrFat, ptr: *const u8, len: u64) {
    if len == 0 {
        // SAFETY: out is a valid out-slot.
        unsafe { write_str_slot(out, b"") };
        return;
    }
    let n: usize = len.try_into().unwrap_or_else(|_| overflow_abort());
    debug_assert!(!ptr.is_null());
    // SAFETY: caller contract — ptr/len describe a readable byte range.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, n) };
    // SAFETY: out is a valid out-slot; `bytes` holds `len` initialized bytes.
    unsafe { write_str_slot(out, bytes) };
}

/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`. `l_ptr`/`r_ptr`
/// point to `l_len`/`r_len` readable bytes (or are null/dangling when
/// the len is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_bytes_concat(
    out: *mut RyoStrFat,
    l_ptr: *const u8,
    l_len: u64,
    r_ptr: *const u8,
    r_len: u64,
) {
    let total = match l_len.checked_add(r_len) {
        Some(t) => t,
        None => overflow_abort(),
    };
    if total == 0 {
        // SAFETY: out is a valid out-slot.
        unsafe {
            *out = RyoStrFat {
                ptr: core::ptr::null_mut(),
                len: 0,
                cap: 0,
            }
        };
        return;
    }
    let l_sz: usize = l_len.try_into().unwrap_or_else(|_| overflow_abort());
    let r_sz: usize = r_len.try_into().unwrap_or_else(|_| overflow_abort());
    if total as usize <= INLINE_CAP {
        // Build inline: write both halves into the slot's data region.
        // SAFETY: out is valid for 24 bytes; total <= 23 fits inline;
        // inputs are readable per the caller contract.
        unsafe {
            let dst = out as *mut u8;
            if l_sz > 0 {
                debug_assert!(!l_ptr.is_null());
                core::ptr::copy_nonoverlapping(l_ptr, dst, l_sz);
            }
            if r_sz > 0 {
                debug_assert!(!r_ptr.is_null());
                core::ptr::copy_nonoverlapping(r_ptr, dst.add(l_sz), r_sz);
            }
            write_inline_tag(out, total);
        }
        return;
    }
    let cap = growth_cap(total);
    let ptr = ryo_bytes_alloc(cap);
    // SAFETY: ptr is freshly allocated for cap >= total bytes; inputs
    // are readable per the caller contract; regions do not overlap.
    unsafe {
        if l_sz > 0 {
            debug_assert!(!l_ptr.is_null());
            core::ptr::copy_nonoverlapping(l_ptr, ptr, l_sz);
        }
        if r_sz > 0 {
            debug_assert!(!r_ptr.is_null());
            core::ptr::copy_nonoverlapping(r_ptr, ptr.add(l_sz), r_sz);
        }
        *out = RyoStrFat {
            ptr,
            len: total,
            cap,
        };
    }
}

/// # Safety
/// `a_ptr` must point to `a_len` readable bytes (or be null/dangling if
/// `a_len == 0`). Same for `b_ptr`/`b_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_bytes_eq(
    a_ptr: *const u8,
    a_len: u64,
    b_ptr: *const u8,
    b_len: u64,
) -> u8 {
    // SAFETY: caller contract forwarded to `ryo_str_eq`.
    unsafe { ryo_str_eq(a_ptr, a_len, b_ptr, b_len) }
}

/// Runtime backing for `bytes_push(b: inout bytes, x: int)` (M8.4.2
/// stopgap: the byte is an `int`, range-checked here; becomes `u8` at
/// M17.1). Appends a SINGLE byte. Panics (exit 101) when `byte > 255`.
///
/// # Safety
/// `s_ptr` points to a valid `RyoStrFat` owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ryo_bytes_push(s_ptr: *mut RyoStrFat, byte: u64) {
    if byte > 255 {
        slice_fail("bytes_push value out of range (0-255)");
    }
    let b = byte as u8;
    // SAFETY: `&b` is readable for 1 byte for the duration of the call;
    // the single-byte append rides `__ryo_str_push`'s growth logic.
    unsafe { __ryo_str_push(s_ptr, &b as *const u8, 1) };
}

/// Runtime backing for M8.4.2 `bytes`/`bytesview` indexing (`b[i]`).
/// Panics (exit 101) on out-of-range. (Negative `int` indices arrive
/// here as huge `u64`s and fail the same check.)
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ryo_bytes_index(ptr: *const u8, len: u64, idx: u64) -> u64 {
    if idx >= len {
        slice_fail("index out of range");
    }
    // SAFETY: idx < len checked above; caller guarantees len readable bytes.
    unsafe { *ptr.add(idx as usize) as u64 }
}

/// `bytes.to_str()` backing (M8.4.2 stopgap): validates UTF-8 and
/// writes an owned `str` copy as a tagged slot; panics (exit 101) on
/// invalid input until M13 turns the signature into `Utf8Error!str`.
///
/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`. `ptr` must point
/// to `len` readable bytes (or be null/dangling when `len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ryo_bytes_to_str(out: *mut RyoStrFat, ptr: *const u8, len: u64) {
    if len == 0 {
        // SAFETY: out is a valid out-slot.
        unsafe { write_str_slot(out, b"") };
        return;
    }
    debug_assert!(!ptr.is_null());
    let n: usize = len.try_into().unwrap_or_else(|_| overflow_abort());
    // SAFETY: caller contract — ptr/len describe a readable byte range.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, n) };
    if core::str::from_utf8(bytes).is_err() {
        slice_fail("bytes are not valid UTF-8");
    }
    // SAFETY: out is a valid out-slot; `bytes` holds `len` initialized bytes.
    unsafe { write_str_slot(out, bytes) };
}

/// `str.to_bytes()` backing (M8.4.2): owned copy of the UTF-8 bytes,
/// written as a tagged slot. Never fails.
///
/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`. `ptr` must point
/// to `len` readable bytes (or be null/dangling when `len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ryo_str_to_bytes(out: *mut RyoStrFat, ptr: *const u8, len: u64) {
    if len == 0 {
        // SAFETY: out is a valid out-slot.
        unsafe { write_str_slot(out, b"") };
        return;
    }
    let n: usize = len.try_into().unwrap_or_else(|_| overflow_abort());
    debug_assert!(!ptr.is_null());
    // SAFETY: caller contract — ptr/len describe a readable byte range.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, n) };
    // SAFETY: out is a valid out-slot; `bytes` holds `len` initialized bytes.
    unsafe { write_str_slot(out, bytes) };
}

/// `print(bytes)` backing (M8.4.2): render the escaped repr as a fresh
/// owned `str`, written as a tagged slot. Printable ASCII (0x20..=0x7E
/// except `\` and `"`) is shown literally; the short escapes
/// `\n \t \r \0 \\ \"` are used where they exist; every other byte
/// renders as `\xNN` (lowercase hex); the result is wrapped in `b"..."`.
///
/// The slot is verbatim heap even when the repr would fit inline: the
/// worst-case buffer is allocated up front and written in place, so the
/// slot reports the real allocation cap (`4*len+3`) rather than routing
/// the tail through a second copy.
///
/// # Safety
/// `out` points to a valid, uninitialized `RyoStrFat`. `ptr` must point
/// to `len` readable bytes (or be null/dangling when `len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ryo_bytes_repr(out: *mut RyoStrFat, ptr: *const u8, len: u64) {
    let n: usize = len.try_into().unwrap_or_else(|_| overflow_abort());
    // Worst case: 3 fixed bytes (`b"`, `"`) + 4 per input byte (`\xNN`).
    let cap = match len.checked_mul(4).and_then(|m| m.checked_add(3)) {
        Some(c) => c,
        None => overflow_abort(),
    };
    let buf = ryo_str_alloc(cap);
    let mut w = 0usize;
    // SAFETY: every write stays within `cap` (≤ 4 per byte + 3 fixed),
    // and reads cover the caller-guaranteed `len` bytes.
    unsafe {
        let push = |buf: *mut u8, w: &mut usize, b: u8| {
            *buf.add(*w) = b;
            *w += 1;
        };
        push(buf, &mut w, b'b');
        push(buf, &mut w, b'"');
        for i in 0..n {
            debug_assert!(!ptr.is_null());
            let byte = *ptr.add(i);
            match byte {
                b'\n' => {
                    push(buf, &mut w, b'\\');
                    push(buf, &mut w, b'n');
                }
                b'\t' => {
                    push(buf, &mut w, b'\\');
                    push(buf, &mut w, b't');
                }
                b'\r' => {
                    push(buf, &mut w, b'\\');
                    push(buf, &mut w, b'r');
                }
                0 => {
                    push(buf, &mut w, b'\\');
                    push(buf, &mut w, b'0');
                }
                b'\\' => {
                    push(buf, &mut w, b'\\');
                    push(buf, &mut w, b'\\');
                }
                b'"' => {
                    push(buf, &mut w, b'\\');
                    push(buf, &mut w, b'"');
                }
                0x20..=0x7e => push(buf, &mut w, byte),
                _ => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    push(buf, &mut w, b'\\');
                    push(buf, &mut w, b'x');
                    push(buf, &mut w, HEX[(byte >> 4) as usize]);
                    push(buf, &mut w, HEX[(byte & 0xf) as usize]);
                }
            }
        }
        push(buf, &mut w, b'"');
    }
    // Verbatim heap slot: report the real allocation cap (not `len`) so
    // `ryo_str_free` and future growth see the true buffer size.
    // SAFETY: out is a valid out-slot; buf is a heap allocation of `cap`
    // bytes holding `w` initialized bytes.
    unsafe {
        *out = RyoStrFat {
            ptr: buf,
            len: w as u64,
            cap,
        };
    }
}

#[cfg(test)]
mod tests;
