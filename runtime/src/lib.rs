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
}
