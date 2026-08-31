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
    debug_assert!(!ptr.is_null());
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
        debug_assert!(!ptr.is_null());
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

/// Return-value packing for the string-producing runtime functions
/// (Phase 0 ABI modernization): `{ptr, len}` is returned as one
/// `u128` (lo = ptr, hi = len).
///
/// Why packed `u128` instead of a struct: this crate is compiled by
/// rustc, and a 24-byte `RyoStrFat` return lowers to a hidden sret
/// pointer on every supported target, while a 16-byte `{ptr, len}`
/// struct still srets under the MSVC x64 ABI. `u128` under the Rust
/// ABI returns in registers everywhere (rax:rdx on x86-64 SysV and
/// Win64, x0:x1 on aarch64) — the convention Cranelift's
/// SystemV/WindowsFastcall/AppleAarch64 return tables match. These
/// functions are therefore `#[unsafe(no_mangle)] pub fn` (Rust ABI),
/// not `extern "C"`; the build.rs lockstep rebuild keeps this crate
/// and the compiler on the same rustc, so the unstable Rust ABI
/// cannot drift within a build.
///
/// `cap` is deliberately NOT in the return value: it is derivable at
/// the call site — 0 for `ryo_str_from_literal` (the static .rodata
/// sentinel) and `len` for every allocating producer below (none of
/// them over-allocates; `__ryo_str_push` manages growth capacity
/// through its unchanged slot ABI). A producer that ever needs
/// `cap != len` must change this ABI.
#[inline]
fn pack_pair(ptr: *mut u8, len: u64) -> u128 {
    ((len as u128) << 64) | (ptr as usize as u128)
}

#[cfg(test)]
fn unpack_pair(v: u128) -> (*mut u8, u64) {
    (v as u64 as *mut u8, (v >> 64) as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn ryo_str_alloc(cap: u64) -> *mut u8 {
    if cap == 0 {
        return core::ptr::null_mut();
    }
    let size: usize = cap.try_into().unwrap_or_else(|_| oom_abort());
    // SAFETY: malloc is called with a nonzero size.
    let ptr = unsafe { c_malloc(size) as *mut u8 };
    if ptr.is_null() {
        oom_abort();
    }
    ptr
}

/// # Safety
/// `ptr` must have been returned by `ryo_str_alloc` or `ryo_str_realloc`
/// with the given `cap`, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_str_free(ptr: *mut u8, cap: u64) {
    if ptr.is_null() || cap == 0 {
        return;
    }
    // SAFETY: caller contract — ptr came from ryo_str_alloc/realloc.
    unsafe { c_free(ptr as *mut c_void) };
}

/// # Safety
/// `ptr` must have been returned by `ryo_str_alloc` or `ryo_str_realloc`
/// with the given `old_cap`, or be null.
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
    let new_size: usize = new_cap.try_into().unwrap_or_else(|_| oom_abort());
    // SAFETY: ptr came from a prior alloc per our # Safety doc; new_size > 0 checked above.
    let new_ptr = unsafe { c_realloc(ptr as *mut c_void, new_size) as *mut u8 };
    if new_ptr.is_null() {
        oom_abort();
    }
    new_ptr
}

/// Helper for fixed-string results (nan, inf, etc.): heap-copy `s` and
/// return the packed pair.
fn str_pair_from_bytes(s: &[u8]) -> u128 {
    let ptr = ryo_str_alloc(s.len() as u64);
    // SAFETY: ptr is freshly allocated for s.len() bytes; s.as_ptr() is
    // readable for the same length; the regions do not overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), ptr, s.len());
    }
    pack_pair(ptr, s.len() as u64)
}

fn oom_abort() -> ! {
    let msg = b"ryo: out of memory\n";
    write_all(STDERR_FD, msg.as_ptr(), msg.len());
    // SAFETY: abort never returns.
    unsafe { abort() }
}

/// # Safety
/// `data` must point to `len` readable bytes (or be dangling when `len == 0`).
#[unsafe(no_mangle)]
pub unsafe fn ryo_str_from_literal(data: *const u8, len: u64) -> u128 {
    if len == 0 {
        return pack_pair(core::ptr::null_mut(), 0);
    }
    // Point directly into rodata; the cap=0 static sentinel is derived
    // at the call site (see `pack_pair` docs).
    pack_pair(data as *mut u8, len)
}

/// Materialize an owned `str` copy from a `strview` (M8.4.1.2). The
/// result owns a fresh heap buffer of exactly `len` bytes; `len == 0`
/// yields the empty `{null, 0}` pair.
///
/// # Safety
/// `ptr` must point to `len` readable bytes — or be null/dangling when
/// `len == 0`.
#[unsafe(no_mangle)]
pub unsafe fn ryo_str_from_view(ptr: *const u8, len: u64) -> u128 {
    if len == 0 {
        return pack_pair(core::ptr::null_mut(), 0);
    }
    let n: usize = len.try_into().unwrap_or_else(|_| oom_abort());
    let buf = ryo_str_alloc(len);
    debug_assert!(!ptr.is_null());
    // SAFETY: caller contract — ptr/len describe a readable byte range;
    // buf is freshly allocated for len bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(ptr, buf, n);
    }
    pack_pair(buf, len)
}

fn slice_fail(msg: &str) -> ! {
    // Raw message + newline, exit 101 — same contract as `ryo_panic`.
    write_all(STDERR_FD, msg.as_ptr(), msg.len());
    write_all(STDERR_FD, b"\n".as_ptr(), 1);
    // SAFETY: exit never returns.
    unsafe { exit(101) }
}

/// True when byte offset `i` in `s[..len]` lies on a UTF-8 char
/// boundary (start, end, or a non-continuation byte).
///
/// # Safety
/// `s` must point to `len` readable bytes (or be null/dangling if `len == 0`).
unsafe fn is_char_boundary(s: *const u8, len: u64, i: u64) -> bool {
    if i == 0 || i == len {
        return true;
    }
    // SAFETY: caller contract — s points to len readable bytes; 0 < i < len here.
    let b = unsafe { *s.add(i as usize) };
    b & 0xC0 != 0x80
}

/// Runtime backing for M8.4 `str` slicing (`s[start:end]`). Out-of-range
/// and non-boundary indices panic at slice creation (final spec §3.1);
/// panic here means stderr message + exit 101, matching `__ryo_panic`.
///
/// Load-bearing invariant: the returned `ptr` is NULL when the
/// requested range is empty *and* the base is empty, and every consumer
/// guards on `len == 0` before dereferencing — so the packed `ptr` may
/// be null whenever the viewed length is 0.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (or be null if `len == 0`).
/// Panics (exit 101) when `start > end`, `end > len`, or either bound
/// is not a UTF-8 char boundary.
#[unsafe(no_mangle)]
pub unsafe fn __ryo_slice(ptr: *const u8, len: u64, start: u64, end: u64) -> u128 {
    if start > end || end > len {
        slice_fail("slice index out of range");
    }
    // SAFETY: caller contract — ptr points to len readable bytes.
    let bounds_ok = unsafe { is_char_boundary(ptr, len, start) && is_char_boundary(ptr, len, end) };
    if !bounds_ok {
        slice_fail("slice index is not a UTF-8 char boundary");
    }
    // SAFETY: `start <= end <= len` checked above, so ptr.add(start)
    // stays within (or one past) the base allocation.
    let out_ptr = unsafe {
        if len == 0 {
            core::ptr::null()
        } else {
            ptr.add(start as usize)
        }
    };
    pack_pair(out_ptr as *mut u8, end - start)
}

/// # Safety
/// `l_ptr` must point to `l_len` readable bytes (or be null/dangling if
/// `l_len == 0`). Same for `r_ptr`/`r_len`.
#[unsafe(no_mangle)]
pub unsafe fn ryo_str_concat(l_ptr: *const u8, l_len: u64, r_ptr: *const u8, r_len: u64) -> u128 {
    let total = match l_len.checked_add(r_len) {
        Some(t) => t,
        None => oom_abort(),
    };
    if total == 0 {
        return pack_pair(core::ptr::null_mut(), 0);
    }
    let l_sz: usize = l_len.try_into().unwrap_or_else(|_| oom_abort());
    let r_sz: usize = r_len.try_into().unwrap_or_else(|_| oom_abort());
    let _: usize = total.try_into().unwrap_or_else(|_| oom_abort());
    let ptr = ryo_str_alloc(total);
    // SAFETY: caller contract — the input buffers are valid for reading
    // and ptr is freshly allocated for total bytes; the copies do not
    // overlap the destination.
    unsafe {
        if l_sz > 0 {
            debug_assert!(!l_ptr.is_null());
            core::ptr::copy_nonoverlapping(l_ptr, ptr, l_sz);
        }
        if r_sz > 0 {
            debug_assert!(!r_ptr.is_null());
            core::ptr::copy_nonoverlapping(r_ptr, ptr.add(l_sz), r_sz);
        }
    }
    pack_pair(ptr, total)
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
        let new_len = match cur_len.checked_add(add) {
            Some(l) => l,
            None => oom_abort(),
        };

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
                    let n: usize = cur_len.try_into().unwrap_or_else(|_| oom_abort());
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
            let dst_off: usize = cur_len.try_into().unwrap_or_else(|_| oom_abort());
            let n: usize = add.try_into().unwrap_or_else(|_| oom_abort());
            debug_assert!(!suffix_ptr.is_null());
            core::ptr::copy_nonoverlapping(suffix_ptr, buf.add(dst_off), n);
        }
        (*s_ptr).ptr = buf;
        (*s_ptr).len = new_len;
        (*s_ptr).cap = cap;
    }
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

#[unsafe(no_mangle)]
pub fn ryo_int_to_str(value: i64) -> u128 {
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
    let len = (buf.len() - pos) as u64;
    let ptr = ryo_str_alloc(len);
    // SAFETY: ptr is newly allocated for len bytes; buf is readable from
    // pos onward for len bytes; the regions do not overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr().add(pos), ptr, len as usize);
    }
    pack_pair(ptr, len)
}

#[unsafe(no_mangle)]
pub fn ryo_float_to_str(value: f64) -> u128 {
    if value.is_nan() {
        return str_pair_from_bytes(b"nan");
    }
    if value.is_infinite() {
        return if value < 0.0 {
            str_pair_from_bytes(b"-inf")
        } else {
            str_pair_from_bytes(b"inf")
        };
    }

    let mut buf = ryu::Buffer::new();
    str_pair_from_bytes(buf.format(value).as_bytes())
}

#[unsafe(no_mangle)]
pub fn ryo_bool_to_str(value: u8) -> u128 {
    str_pair_from_bytes(if value != 0 { b"true" } else { b"false" })
}

// ---------- bytes (M8.4.2) ----------
//
// Owned `bytes` buffers mirror the `str` ABI exactly: producers return
// `{ptr, len}` packed in one `u128` (see `pack_pair`), `cap` is derived
// at the call site (0 for literals, len for allocating producers), and
// `__ryo_bytes_push` manages growth through the same 24-byte slot ABI.
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

/// # Safety
/// `data` must point to `len` readable bytes (or be dangling when `len == 0`).
#[unsafe(no_mangle)]
pub unsafe fn ryo_bytes_from_literal(data: *const u8, len: u64) -> u128 {
    if len == 0 {
        return pack_pair(core::ptr::null_mut(), 0);
    }
    // Point directly into rodata; the cap=0 static sentinel is derived
    // at the call site (see `pack_pair` docs).
    pack_pair(data as *mut u8, len)
}

/// Materialize an owned `bytes` copy from a `bytesview` (M8.4.2). The
/// result owns a fresh heap buffer of exactly `len` bytes; `len == 0`
/// yields the empty `{null, 0}` pair.
///
/// # Safety
/// `ptr` must point to `len` readable bytes — or be null/dangling when
/// `len == 0`.
#[unsafe(no_mangle)]
pub unsafe fn ryo_bytes_from_view(ptr: *const u8, len: u64) -> u128 {
    if len == 0 {
        return pack_pair(core::ptr::null_mut(), 0);
    }
    let n: usize = len.try_into().unwrap_or_else(|_| oom_abort());
    let buf = ryo_bytes_alloc(len);
    debug_assert!(!ptr.is_null());
    // SAFETY: caller contract — ptr/len describe a readable byte range;
    // buf is freshly allocated for len bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(ptr, buf, n);
    }
    pack_pair(buf, len)
}

/// # Safety
/// `l_ptr` must point to `l_len` readable bytes (or be null/dangling if
/// `l_len == 0`). Same for `r_ptr`/`r_len`.
#[unsafe(no_mangle)]
pub unsafe fn ryo_bytes_concat(l_ptr: *const u8, l_len: u64, r_ptr: *const u8, r_len: u64) -> u128 {
    let total = match l_len.checked_add(r_len) {
        Some(t) => t,
        None => oom_abort(),
    };
    if total == 0 {
        return pack_pair(core::ptr::null_mut(), 0);
    }
    let l_sz: usize = l_len.try_into().unwrap_or_else(|_| oom_abort());
    let r_sz: usize = r_len.try_into().unwrap_or_else(|_| oom_abort());
    let _: usize = total.try_into().unwrap_or_else(|_| oom_abort());
    let ptr = ryo_bytes_alloc(total);
    // SAFETY: caller contract — the input buffers are valid for reading
    // and ptr is freshly allocated for total bytes; the copies do not
    // overlap the destination.
    unsafe {
        if l_sz > 0 {
            debug_assert!(!l_ptr.is_null());
            core::ptr::copy_nonoverlapping(l_ptr, ptr, l_sz);
        }
        if r_sz > 0 {
            debug_assert!(!r_ptr.is_null());
            core::ptr::copy_nonoverlapping(r_ptr, ptr.add(l_sz), r_sz);
        }
    }
    pack_pair(ptr, total)
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

/// Runtime backing for M8.4.2 `bytes` slicing (`b[start:end]`).
/// Bounds-checked like `__ryo_slice`, but WITHOUT the UTF-8 char-boundary
/// check — the single behavioral divergence from `strview`.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (or be null if `len == 0`).
/// Panics (exit 101) when `start > end` or `end > len`.
#[unsafe(no_mangle)]
pub unsafe fn __ryo_bytes_slice(ptr: *const u8, len: u64, start: u64, end: u64) -> u128 {
    if start > end || end > len {
        slice_fail("slice index out of range");
    }
    // SAFETY: `start <= end <= len` checked above, so ptr.add(start)
    // stays within (or one past) the base allocation.
    let out_ptr = unsafe {
        if len == 0 {
            core::ptr::null()
        } else {
            ptr.add(start as usize)
        }
    };
    pack_pair(out_ptr as *mut u8, end - start)
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
/// returns an owned `str` copy; panics (exit 101) on invalid input
/// until M13 turns the signature into `Utf8Error!str`.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (or be null/dangling when
/// `len == 0`).
#[unsafe(no_mangle)]
pub unsafe fn __ryo_bytes_to_str(ptr: *const u8, len: u64) -> u128 {
    if len == 0 {
        return pack_pair(core::ptr::null_mut(), 0);
    }
    debug_assert!(!ptr.is_null());
    // SAFETY: caller contract — ptr/len describe a readable byte range.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    if core::str::from_utf8(bytes).is_err() {
        slice_fail("bytes are not valid UTF-8");
    }
    let n: usize = len.try_into().unwrap_or_else(|_| oom_abort());
    let buf = ryo_str_alloc(len);
    // SAFETY: buf is freshly allocated for len bytes; regions disjoint.
    unsafe {
        core::ptr::copy_nonoverlapping(ptr, buf, n);
    }
    pack_pair(buf, len)
}

/// `str.to_bytes()` backing (M8.4.2): owned copy of the UTF-8 bytes.
/// Never fails.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (or be null/dangling when
/// `len == 0`).
#[unsafe(no_mangle)]
pub unsafe fn ryo_str_to_bytes(ptr: *const u8, len: u64) -> u128 {
    if len == 0 {
        return pack_pair(core::ptr::null_mut(), 0);
    }
    let n: usize = len.try_into().unwrap_or_else(|_| oom_abort());
    let buf = ryo_bytes_alloc(len);
    debug_assert!(!ptr.is_null());
    // SAFETY: caller contract; buf is freshly allocated for len bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(ptr, buf, n);
    }
    pack_pair(buf, len)
}

/// `print(bytes)` backing (M8.4.2): render the escaped repr as a fresh
/// owned `str`. Printable ASCII (0x20..=0x7E except `\` and `"`) is
/// shown literally; the short escapes `\n \t \r \0 \\ \"` are used
/// where they exist; every other byte renders as `\xNN` (lowercase
/// hex); the result is wrapped in `b"..."`.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (or be null/dangling when
/// `len == 0`).
#[unsafe(no_mangle)]
pub unsafe fn __ryo_bytes_repr(ptr: *const u8, len: u64) -> u128 {
    let n: usize = len.try_into().unwrap_or_else(|_| oom_abort());
    // Worst case: 3 fixed bytes (`b"`, `"`) + 4 per input byte (`\xNN`).
    let cap = match len.checked_mul(4).and_then(|m| m.checked_add(3)) {
        Some(c) => c,
        None => oom_abort(),
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
    // `cap` is derived at the call site as `len` (LenIsCap); the actual
    // allocation is larger, which is harmless — `ryo_str_free` only
    // reads `cap == 0` as the static sentinel.
    pack_pair(buf, w as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alloc_and_free() {
        unsafe {
            let ptr = ryo_str_alloc(16);
            assert!(!ptr.is_null());
            ryo_str_free(ptr, 16);
        }
    }

    #[test]
    fn test_alloc_zero_returns_null() {
        let ptr = ryo_str_alloc(0);
        assert!(ptr.is_null());
    }

    #[test]
    fn test_free_null_is_noop() {
        unsafe { ryo_str_free(core::ptr::null_mut(), 0) };
    }

    #[test]
    fn test_realloc_grow() {
        unsafe {
            let ptr = ryo_str_alloc(8);
            assert!(!ptr.is_null());
            let ptr2 = ryo_str_realloc(ptr, 8, 32);
            assert!(!ptr2.is_null());
            ryo_str_free(ptr2, 32);
        }
    }

    #[test]
    fn test_realloc_from_null() {
        unsafe {
            let ptr = ryo_str_realloc(core::ptr::null_mut(), 0, 16);
            assert!(!ptr.is_null());
            ryo_str_free(ptr, 16);
        }
    }

    #[test]
    fn test_realloc_to_zero() {
        unsafe {
            let ptr = ryo_str_alloc(16);
            assert!(!ptr.is_null());
            let ptr2 = ryo_str_realloc(ptr, 16, 0);
            assert!(ptr2.is_null());
        }
    }

    #[test]
    fn test_from_literal_nonempty() {
        let data = b"hello";
        // SAFETY: data points to 5 readable bytes.
        let pair = unsafe { ryo_str_from_literal(data.as_ptr(), 5) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert_eq!(out_ptr as *const u8, data.as_ptr());
        assert_eq!(out_len, 5);
        // cap is 0 by ABI convention (the static sentinel never reaches
        // the runtime).
        // SAFETY: the pair points into the readable literal bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"hello");
    }

    #[test]
    fn test_from_literal_returns_static_pointer() {
        let data = b"hello";
        // SAFETY: data points to 5 readable bytes.
        let pair = unsafe { ryo_str_from_literal(data.as_ptr(), 5) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert_eq!(out_ptr as *const u8, data.as_ptr());
        assert_eq!(out_len, 5);
        // cap is 0 by ABI convention (the static sentinel never reaches
        // the runtime).
    }

    #[test]
    fn test_free_static_str_is_noop() {
        let data = b"hello";
        // SAFETY: data points to 5 readable bytes.
        let pair = unsafe { ryo_str_from_literal(data.as_ptr(), 5) };
        let (out_ptr, _) = unpack_pair(pair);
        // Static sentinel: cap = 0 by ABI convention, so free is a noop.
        // SAFETY: out_ptr is a static .rodata pointer freed with cap 0.
        unsafe { ryo_str_free(out_ptr, 0) };
    }

    #[test]
    fn test_from_literal_empty() {
        // SAFETY: len == 0, so the data pointer is never dereferenced.
        let pair = unsafe { ryo_str_from_literal(b"".as_ptr(), 0) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert!(out_ptr.is_null());
        assert_eq!(out_len, 0);
        // cap is 0 by ABI convention (the static sentinel never reaches
        // the runtime).
    }

    #[test]
    fn test_concat_two_strings() {
        // SAFETY: both input buffers are valid for reading.
        let pair = unsafe { ryo_str_concat(b"Hello, ".as_ptr(), 7, b"World!".as_ptr(), 6) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert_eq!(out_len, 13);
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"Hello, World!");
        // cap == len for allocating producers (codegen-side derivation).
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_concat_empty_left() {
        // SAFETY: both input buffers are valid for reading.
        let pair = unsafe { ryo_str_concat(b"".as_ptr(), 0, b"abc".as_ptr(), 3) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert_eq!(out_len, 3);
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"abc");
        // cap == len for allocating producers (codegen-side derivation).
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_concat_both_empty() {
        // SAFETY: len == 0 on both sides, so neither pointer is dereferenced.
        let pair = unsafe { ryo_str_concat(core::ptr::null(), 0, core::ptr::null(), 0) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert!(out_ptr.is_null());
        assert_eq!(out_len, 0);
    }

    #[test]
    fn test_eq_same_content() {
        let result = unsafe { ryo_str_eq(b"hello".as_ptr(), 5, b"hello".as_ptr(), 5) };
        assert_eq!(result, 1);
    }

    #[test]
    fn test_eq_different_content() {
        let result = unsafe { ryo_str_eq(b"hello".as_ptr(), 5, b"world".as_ptr(), 5) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_eq_both_empty() {
        let result = unsafe { ryo_str_eq(core::ptr::null(), 0, core::ptr::null(), 0) };
        assert_eq!(result, 1);
    }

    #[test]
    fn test_eq_different_lengths() {
        let result = unsafe { ryo_str_eq(b"hi".as_ptr(), 2, b"hello".as_ptr(), 5) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_int_to_str_positive() {
        let (out_ptr, out_len) = unpack_pair(ryo_int_to_str(42));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"42");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_int_to_str_negative() {
        let (out_ptr, out_len) = unpack_pair(ryo_int_to_str(-123));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"-123");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_int_to_str_zero() {
        let (out_ptr, out_len) = unpack_pair(ryo_int_to_str(0));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"0");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_int_to_str_min() {
        let (out_ptr, out_len) = unpack_pair(ryo_int_to_str(i64::MIN));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"-9223372036854775808");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_float_to_str_nan() {
        let (out_ptr, out_len) = unpack_pair(ryo_float_to_str(f64::NAN));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"nan");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_float_to_str_inf() {
        let (out_ptr, out_len) = unpack_pair(ryo_float_to_str(f64::INFINITY));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"inf");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_float_to_str_neg_inf() {
        let (out_ptr, out_len) = unpack_pair(ryo_float_to_str(f64::NEG_INFINITY));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"-inf");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_float_to_str() {
        let (out_ptr, out_len) = unpack_pair(ryo_float_to_str(2.75));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        let s = core::str::from_utf8(slice).unwrap();
        assert!(s.starts_with("2.75"), "got: {}", s);
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_float_to_str_large_value() {
        // Value larger than u64::MAX — old code would saturate
        let (out_ptr, out_len) = unpack_pair(ryo_float_to_str(1.8e19));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        let s = core::str::from_utf8(slice).unwrap();
        let parsed: f64 = s.parse().unwrap();
        assert_eq!(parsed, 1.8e19);
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_float_to_str_precision() {
        let (out_ptr, out_len) = unpack_pair(ryo_float_to_str(0.1 + 0.2));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        let s = core::str::from_utf8(slice).unwrap();
        let parsed: f64 = s.parse().unwrap();
        assert_eq!(parsed, 0.1 + 0.2);
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_bool_to_str_true() {
        let (out_ptr, out_len) = unpack_pair(ryo_bool_to_str(1));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"true");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_bool_to_str_false() {
        let (out_ptr, out_len) = unpack_pair(ryo_bool_to_str(0));
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"false");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn test_concat_static_left_heap_right() {
        unsafe {
            // Simulate: "Hello, " + heap_string
            let left = b"Hello, ";
            let left_fat = RyoStrFat {
                ptr: left.as_ptr() as *mut u8,
                len: 7,
                cap: 0, // static
            };

            // Create a heap string for the right side
            let mut right_fat = RyoStrFat {
                ptr: core::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            let right_data = b"World!";
            let right_ptr = ryo_str_alloc(6);
            core::ptr::copy_nonoverlapping(right_data.as_ptr(), right_ptr, 6);
            right_fat.ptr = right_ptr;
            right_fat.len = 6;
            right_fat.cap = 6;

            let pair = ryo_str_concat(left_fat.ptr, left_fat.len, right_fat.ptr, right_fat.len);
            let (out_ptr, out_len) = unpack_pair(pair);

            assert_eq!(out_len, 13);
            // cap == len for allocating producers (codegen-side derivation).
            let slice = core::slice::from_raw_parts(out_ptr, out_len as usize);
            assert_eq!(slice, b"Hello, World!");

            // Free: static left is safe (cap=0 → noop), heap right and result freed
            ryo_str_free(left_fat.ptr, left_fat.cap);
            ryo_str_free(right_fat.ptr, right_fat.cap);
            ryo_str_free(out_ptr, out_len);
        }
    }

    #[test]
    fn slice_basic() {
        let s = "héllo wörld".as_bytes();
        // "héllo" is 6 bytes (é = 2 bytes)
        // SAFETY: s is readable for its byte length;
        // the range 0..6 is in-bounds (see above).
        let pair = unsafe { __ryo_slice(s.as_ptr(), s.len() as u64, 0, 6) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert_eq!(out_len, 6);
        // SAFETY: __ryo_slice returned a valid view into s for out_len bytes.
        let got = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(got, "héllo".as_bytes());
    }

    #[test]
    fn slice_empty_at_len_is_ok() {
        let s = "abc".as_bytes();
        // SAFETY: "abc" provides three readable bytes;
        // start == end == len is the empty-at-end case the ABI allows.
        let pair = unsafe { __ryo_slice(s.as_ptr(), 3, 3, 3) };
        let (_, out_len) = unpack_pair(pair);
        assert_eq!(out_len, 0);
    }

    #[test]
    fn slice_nonzero_offset() {
        let s = "héllo wörld".as_bytes();
        // "wörld" starts at byte 7 (h=1, é=2, "llo "=4) and is 6 bytes
        // — exercises the non-zero pointer-offset path.
        // SAFETY: s is readable for its byte length;
        // the range 7..13 is in-bounds (see above).
        let pair = unsafe { __ryo_slice(s.as_ptr(), s.len() as u64, 7, 13) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert_eq!(out_len, 6);
        // SAFETY: __ryo_slice returned a valid view into s for out_len bytes.
        let got = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(got, "wörld".as_bytes());
    }

    #[test]
    fn str_from_view_copies_bytes() {
        let src = b"hello";
        // SAFETY: src points to 5 readable bytes.
        let pair = unsafe { ryo_str_from_view(src.as_ptr(), 5) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert_eq!(out_len, 5);
        // cap == len for allocating producers (codegen-side derivation).
        // SAFETY: the pair points to a freshly allocated buffer of out_len bytes.
        let slice = unsafe { core::slice::from_raw_parts(out_ptr, out_len as usize) };
        assert_eq!(slice, b"hello");
        // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
        unsafe { ryo_str_free(out_ptr, out_len) };
    }

    #[test]
    fn str_from_view_buffer_is_independent() {
        unsafe {
            // Heap-backed source: the copy must own a fresh buffer.
            let src = ryo_str_alloc(3);
            core::ptr::copy_nonoverlapping(b"abc".as_ptr(), src, 3);
            let pair = ryo_str_from_view(src, 3);
            let (out_ptr, out_len) = unpack_pair(pair);
            assert!(
                !core::ptr::eq(out_ptr, src),
                "copy must not alias the source"
            );
            // Overwrite and free the source; the copy is unaffected.
            core::ptr::write_bytes(src, b'x', 3);
            ryo_str_free(src, 3);
            let slice = core::slice::from_raw_parts(out_ptr, out_len as usize);
            assert_eq!(slice, b"abc");
            // SAFETY: out_ptr came from ryo_str_alloc with capacity out_len.
            ryo_str_free(out_ptr, out_len);
        }
    }

    #[test]
    fn str_from_view_empty() {
        // ptr may be null/dangling when len == 0 (`ryo_str_from_view` invariant).
        // SAFETY: len == 0, so the pointer is never dereferenced.
        let pair = unsafe { ryo_str_from_view(core::ptr::null(), 0) };
        let (out_ptr, out_len) = unpack_pair(pair);
        assert!(out_ptr.is_null());
        assert_eq!(out_len, 0);
    }

    #[test]
    fn print_smoke_writes_to_stdout() {
        // Smoke test only: asserts no crash on the happy path and on the
        // len==0 / null-ptr edge. Output bytes themselves are verified
        // end-to-end by the compiler integration tests.
        unsafe { ryo_print(b"ryo-print-smoke\n".as_ptr(), 16) };
        unsafe { ryo_print(core::ptr::null(), 0) };
    }

    #[test]
    fn bytes_concat_combines() {
        let a = [0x01u8, 0x02];
        let b = [0x03u8];
        let v = unsafe { ryo_bytes_concat(a.as_ptr(), a.len() as u64, b.as_ptr(), b.len() as u64) };
        let (p, l) = unpack_pair(v);
        assert_eq!(l, 3);
        let s = unsafe { core::slice::from_raw_parts(p, l as usize) };
        assert_eq!(s, &[0x01, 0x02, 0x03]);
        unsafe { ryo_bytes_free(p, l) };
    }

    #[test]
    fn bytes_concat_empty_is_null_pair() {
        let v = unsafe { ryo_bytes_concat(core::ptr::null(), 0, core::ptr::null(), 0) };
        let (p, l) = unpack_pair(v);
        assert!(p.is_null());
        assert_eq!(l, 0);
    }

    #[test]
    fn bytes_from_view_copies() {
        let src = [0xaau8, 0xbb];
        let v = unsafe { ryo_bytes_from_view(src.as_ptr(), src.len() as u64) };
        let (p, l) = unpack_pair(v);
        assert_eq!(l, 2);
        assert_ne!(p, src.as_ptr() as *mut u8); // independent copy
        let s = unsafe { core::slice::from_raw_parts(p, l as usize) };
        assert_eq!(s, &[0xaa, 0xbb]);
        unsafe { ryo_bytes_free(p, l) };
    }

    #[test]
    fn bytes_slice_returns_subrange() {
        let src = [0x01u8, 0x02, 0x03, 0x04];
        let v = unsafe { __ryo_bytes_slice(src.as_ptr(), 4, 1, 3) };
        let (p, l) = unpack_pair(v);
        assert_eq!(l, 2);
        let s = unsafe { core::slice::from_raw_parts(p, l as usize) };
        assert_eq!(s, &[0x02, 0x03]);
        // View into the source — do NOT free.
    }

    #[test]
    fn bytes_slice_allows_non_char_boundaries() {
        // The single behavioral divergence from `__ryo_slice`: no UTF-8
        // boundary check — slicing mid-codepoint is fine for bytes.
        let src = "héllo".as_bytes(); // é is two bytes at offsets 1..3
        let v = unsafe { __ryo_bytes_slice(src.as_ptr(), src.len() as u64, 1, 3) };
        let (_, l) = unpack_pair(v);
        assert_eq!(l, 2);
    }

    #[test]
    fn bytes_push_appends_and_grows_from_static() {
        let src = [0x01u8];
        let mut fat = RyoStrFat {
            ptr: src.as_ptr() as *mut u8, // static cap=0: NOT heap-owned
            len: 1,
            cap: 0,
        };
        unsafe { __ryo_bytes_push(&mut fat, 0xff) };
        assert_eq!(fat.len, 2);
        assert!(fat.cap >= 2);
        let s = unsafe { core::slice::from_raw_parts(fat.ptr, fat.len as usize) };
        assert_eq!(s, &[0x01, 0xff]);
        unsafe { ryo_bytes_free(fat.ptr, fat.cap) };
    }

    #[test]
    fn bytes_index_reads_byte() {
        let src = [0x00u8, 0x7f, 0xff];
        for (i, want) in src.iter().enumerate() {
            let got = unsafe { __ryo_bytes_index(src.as_ptr(), 3, i as u64) };
            assert_eq!(got, *want as u64);
        }
    }

    #[test]
    fn bytes_eq_compares_contents() {
        let a = [0x01u8, 0x02];
        let b = [0x01u8, 0x02];
        let c = [0x01u8, 0x03];
        assert_eq!(unsafe { ryo_bytes_eq(a.as_ptr(), 2, b.as_ptr(), 2) }, 1);
        assert_eq!(unsafe { ryo_bytes_eq(a.as_ptr(), 2, c.as_ptr(), 2) }, 0);
        assert_eq!(unsafe { ryo_bytes_eq(a.as_ptr(), 1, a.as_ptr(), 2) }, 0);
        assert_eq!(
            unsafe { ryo_bytes_eq(core::ptr::null(), 0, core::ptr::null(), 0) },
            1
        );
    }

    #[test]
    fn bytes_to_str_copies_valid_utf8() {
        let src = "héllo".as_bytes();
        let v = unsafe { __ryo_bytes_to_str(src.as_ptr(), src.len() as u64) };
        let (p, l) = unpack_pair(v);
        let s = unsafe { core::slice::from_raw_parts(p, l as usize) };
        assert_eq!(s, "héllo".as_bytes());
        unsafe { ryo_str_free(p, l) };
    }

    #[test]
    fn str_to_bytes_copies() {
        let src = "héllo".as_bytes();
        let v = unsafe { ryo_str_to_bytes(src.as_ptr(), src.len() as u64) };
        let (p, l) = unpack_pair(v);
        let s = unsafe { core::slice::from_raw_parts(p, l as usize) };
        assert_eq!(s, "héllo".as_bytes());
        unsafe { ryo_bytes_free(p, l) };
    }

    #[test]
    fn bytes_repr_escapes() {
        // A, NUL, 0xff, newline, '"', '\', '~' (0x7e printable), ESC (0x1b)
        let input = [b'A', 0x00, 0xff, b'\n', b'"', b'\\', 0x7e, 0x1b];
        let v = unsafe { __ryo_bytes_repr(input.as_ptr(), input.len() as u64) };
        let (p, l) = unpack_pair(v);
        let s = unsafe { core::slice::from_raw_parts(p, l as usize) };
        assert_eq!(s, b"b\"A\\0\\xff\\n\\\"\\\\~\\x1b\"");
        unsafe { ryo_str_free(p, l) };
    }

    #[test]
    fn bytes_repr_empty() {
        let v = unsafe { __ryo_bytes_repr(core::ptr::null(), 0) };
        let (p, l) = unpack_pair(v);
        let s = unsafe { core::slice::from_raw_parts(p, l as usize) };
        assert_eq!(s, b"b\"\"");
        unsafe { ryo_str_free(p, l) };
    }
}
