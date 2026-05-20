use std::alloc::{Layout, alloc, dealloc, realloc};

#[repr(C)]
pub struct RyoStrFat {
    pub ptr: *mut u8,
    pub len: u64,
    pub cap: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn ryo_str_alloc(cap: u64) -> *mut u8 {
    if cap == 0 {
        return std::ptr::null_mut();
    }
    let layout = layout_for(cap);
    let ptr = unsafe { alloc(layout) };
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
    let layout = layout_for(cap);
    unsafe { dealloc(ptr, layout) };
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
        unsafe { ryo_str_free(ptr, old_cap) };
        return std::ptr::null_mut();
    }
    let layout = layout_for(old_cap);
    let new_ptr = unsafe { realloc(ptr, layout, new_cap as usize) };
    if new_ptr.is_null() {
        oom_abort();
    }
    new_ptr
}

fn layout_for(cap: u64) -> Layout {
    Layout::from_size_align(cap as usize, 1).unwrap_or_else(|_| oom_abort())
}

fn oom_abort() -> ! {
    eprintln!("ryo: out of memory");
    std::process::abort();
}

/// # Safety
/// `data` must point to `len` readable bytes. `out` must point to a valid `RyoStrFat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_str_from_literal(data: *const u8, len: u64, out: *mut RyoStrFat) {
    unsafe {
        if len == 0 {
            (*out).ptr = std::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
            return;
        }
        let ptr = ryo_str_alloc(len);
        if ptr.is_null() {
            oom_abort();
        }
        core::ptr::copy_nonoverlapping(data, ptr, len as usize);
        (*out).ptr = ptr;
        (*out).len = len;
        (*out).cap = len;
    }
}

/// # Safety
/// `l_ptr` must point to `l_len` readable bytes (or be null if l_len==0).
/// Same for `r_ptr`/`r_len`. `out` must point to a valid `RyoStrFat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_str_concat(
    l_ptr: *const u8,
    l_len: u64,
    r_ptr: *const u8,
    r_len: u64,
    out: *mut RyoStrFat,
) {
    unsafe {
        let total = l_len + r_len;
        if total == 0 {
            (*out).ptr = std::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
            return;
        }
        let ptr = ryo_str_alloc(total);
        if ptr.is_null() {
            oom_abort();
        }
        if l_len > 0 {
            core::ptr::copy_nonoverlapping(l_ptr, ptr, l_len as usize);
        }
        if r_len > 0 {
            core::ptr::copy_nonoverlapping(r_ptr, ptr.add(l_len as usize), r_len as usize);
        }
        (*out).ptr = ptr;
        (*out).len = total;
        (*out).cap = total;
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
    let a_slice = unsafe { core::slice::from_raw_parts(a_ptr, a_len as usize) };
    let b_slice = unsafe { core::slice::from_raw_parts(b_ptr, b_len as usize) };
    if a_slice == b_slice { 1 } else { 0 }
}

/// # Safety
/// `out` must point to a valid `RyoStrFat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_int_to_str(value: i64, out: *mut RyoStrFat) {
    let mut buf = [0u8; 20];
    let mut n = value;
    let negative = n < 0;
    if negative {
        n = n.wrapping_neg();
    }
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
    if ptr.is_null() {
        oom_abort();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr().add(pos), ptr, len as usize);
        (*out).ptr = ptr;
        (*out).len = len;
        (*out).cap = len;
    }
}

/// # Safety
/// `out` must point to a valid `RyoStrFat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_float_to_str(value: f64, out: *mut RyoStrFat) {
    let mut buf = [0u8; 64];
    let mut pos = 0usize;

    let negative = value < 0.0;
    let abs_val = if negative { -value } else { value };

    if negative {
        buf[pos] = b'-';
        pos += 1;
    }

    let int_part = abs_val as u64;
    let frac_part = ((abs_val - int_part as f64) * 1_000_000.0 + 0.5) as u64;

    // Write integer part
    let mut int_buf = [0u8; 20];
    let mut int_pos = int_buf.len();
    if int_part == 0 {
        int_pos -= 1;
        int_buf[int_pos] = b'0';
    } else {
        let mut n = int_part;
        while n > 0 {
            int_pos -= 1;
            int_buf[int_pos] = b'0' + (n % 10) as u8;
            n /= 10;
        }
    }
    let int_len = int_buf.len() - int_pos;
    buf[pos..pos + int_len].copy_from_slice(&int_buf[int_pos..]);
    pos += int_len;

    // Write fractional part (trim trailing zeros)
    buf[pos] = b'.';
    pos += 1;
    let mut frac_buf = [0u8; 6];
    let mut f = frac_part;
    for i in (0..6).rev() {
        frac_buf[i] = b'0' + (f % 10) as u8;
        f /= 10;
    }
    let mut frac_len = 6;
    while frac_len > 1 && frac_buf[frac_len - 1] == b'0' {
        frac_len -= 1;
    }
    buf[pos..pos + frac_len].copy_from_slice(&frac_buf[..frac_len]);
    pos += frac_len;

    let len = pos as u64;
    let ptr = ryo_str_alloc(len);
    if ptr.is_null() {
        oom_abort();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, len as usize);
        (*out).ptr = ptr;
        (*out).len = len;
        (*out).cap = len;
    }
}

/// # Safety
/// `out` must point to a valid `RyoStrFat`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ryo_bool_to_str(value: u8, out: *mut RyoStrFat) {
    let s: &[u8] = if value != 0 { b"true" } else { b"false" };
    let ptr = ryo_str_alloc(s.len() as u64);
    if ptr.is_null() {
        oom_abort();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), ptr, s.len());
        (*out).ptr = ptr;
        (*out).len = s.len() as u64;
        (*out).cap = s.len() as u64;
    }
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
        unsafe { ryo_str_free(std::ptr::null_mut(), 0) };
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
            let ptr = ryo_str_realloc(std::ptr::null_mut(), 0, 16);
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
        unsafe {
            let data = b"hello";
            let mut out = RyoStrFat {
                ptr: std::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            ryo_str_from_literal(data.as_ptr(), 5, &mut out);
            assert!(!out.ptr.is_null());
            assert_eq!(out.len, 5);
            assert_eq!(out.cap, 5);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            assert_eq!(slice, b"hello");
            ryo_str_free(out.ptr, out.cap);
        }
    }

    #[test]
    fn test_from_literal_empty() {
        unsafe {
            let mut out = RyoStrFat {
                ptr: std::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            ryo_str_from_literal(b"".as_ptr(), 0, &mut out);
            assert!(out.ptr.is_null());
            assert_eq!(out.len, 0);
            assert_eq!(out.cap, 0);
        }
    }

    #[test]
    fn test_concat_two_strings() {
        unsafe {
            let mut out = RyoStrFat {
                ptr: std::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            ryo_str_concat(b"Hello, ".as_ptr(), 7, b"World!".as_ptr(), 6, &mut out);
            assert_eq!(out.len, 13);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            assert_eq!(slice, b"Hello, World!");
            ryo_str_free(out.ptr, out.cap);
        }
    }

    #[test]
    fn test_concat_empty_left() {
        unsafe {
            let mut out = RyoStrFat {
                ptr: std::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            ryo_str_concat(b"".as_ptr(), 0, b"abc".as_ptr(), 3, &mut out);
            assert_eq!(out.len, 3);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            assert_eq!(slice, b"abc");
            ryo_str_free(out.ptr, out.cap);
        }
    }

    #[test]
    fn test_concat_both_empty() {
        unsafe {
            let mut out = RyoStrFat {
                ptr: std::ptr::null_mut(),
                len: 0,
                cap: 0,
            };
            ryo_str_concat(std::ptr::null(), 0, std::ptr::null(), 0, &mut out);
            assert!(out.ptr.is_null());
            assert_eq!(out.len, 0);
            assert_eq!(out.cap, 0);
        }
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
        let result = unsafe { ryo_str_eq(std::ptr::null(), 0, std::ptr::null(), 0) };
        assert_eq!(result, 1);
    }

    #[test]
    fn test_eq_different_lengths() {
        let result = unsafe { ryo_str_eq(b"hi".as_ptr(), 2, b"hello".as_ptr(), 5) };
        assert_eq!(result, 0);
    }

    #[test]
    fn test_int_to_str_positive() {
        unsafe {
            let mut out = RyoStrFat { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
            ryo_int_to_str(42, &mut out);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            assert_eq!(slice, b"42");
            ryo_str_free(out.ptr, out.cap);
        }
    }

    #[test]
    fn test_int_to_str_negative() {
        unsafe {
            let mut out = RyoStrFat { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
            ryo_int_to_str(-123, &mut out);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            assert_eq!(slice, b"-123");
            ryo_str_free(out.ptr, out.cap);
        }
    }

    #[test]
    fn test_int_to_str_zero() {
        unsafe {
            let mut out = RyoStrFat { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
            ryo_int_to_str(0, &mut out);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            assert_eq!(slice, b"0");
            ryo_str_free(out.ptr, out.cap);
        }
    }

    #[test]
    fn test_float_to_str() {
        unsafe {
            let mut out = RyoStrFat { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
            ryo_float_to_str(2.75, &mut out);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            let s = core::str::from_utf8(slice).unwrap();
            assert!(s.starts_with("2.75"), "got: {}", s);
            ryo_str_free(out.ptr, out.cap);
        }
    }

    #[test]
    fn test_bool_to_str_true() {
        unsafe {
            let mut out = RyoStrFat { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
            ryo_bool_to_str(1, &mut out);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            assert_eq!(slice, b"true");
            ryo_str_free(out.ptr, out.cap);
        }
    }

    #[test]
    fn test_bool_to_str_false() {
        unsafe {
            let mut out = RyoStrFat { ptr: std::ptr::null_mut(), len: 0, cap: 0 };
            ryo_bool_to_str(0, &mut out);
            let slice = core::slice::from_raw_parts(out.ptr, out.len as usize);
            assert_eq!(slice, b"false");
            ryo_str_free(out.ptr, out.cap);
        }
    }
}
