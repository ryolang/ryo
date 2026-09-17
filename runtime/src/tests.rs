use super::*;

/// Read the byte content of a tagged slot, whether inline (bytes in
/// the slot itself) or heap (bytes at `slot.ptr`).
fn slot_content(slot: &RyoStrFat) -> &[u8] {
    if is_inline(slot.cap) {
        let len = inline_len(slot.cap) as usize;
        // SAFETY: an inline slot holds `len` initialized bytes in its
        // data region (offsets 0..len).
        unsafe { core::slice::from_raw_parts(slot as *const RyoStrFat as *const u8, len) }
    } else {
        // SAFETY: a heap slot's ptr is valid for `len` initialized
        // bytes (produced by a slot-out runtime function).
        unsafe { core::slice::from_raw_parts(slot.ptr, slot.len as usize) }
    }
}

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
fn test_free_static_str_is_noop() {
    let data = b"hello";
    // Static sentinel: cap = 0 by ABI convention, so free is a noop —
    // freeing a non-heap .rodata pointer with cap 0 must not touch it.
    // SAFETY: cap 0 makes ryo_str_free return before dereferencing.
    unsafe { ryo_str_free(data.as_ptr() as *mut u8, 0) };
}

#[test]
fn test_concat_two_strings() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; both input buffers are valid for reading.
    unsafe { ryo_str_concat(&mut slot, b"Hello, ".as_ptr(), 7, b"World!".as_ptr(), 6) };
    // 13 bytes fits inline (SSO).
    assert!(is_inline(slot.cap));
    assert_eq!(inline_len(slot.cap), 13);
    assert_eq!(slot_content(&slot), b"Hello, World!");
}

#[test]
fn test_concat_inline_result() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; literals readable for the given lens.
    unsafe { ryo_str_concat(&mut slot, b"user".as_ptr(), 4, b"42".as_ptr(), 2) };
    assert!(is_inline(slot.cap));
    assert_eq!(inline_len(slot.cap), 6);
    let bytes = unsafe { core::slice::from_raw_parts(&slot as *const RyoStrFat as *const u8, 6) };
    assert_eq!(bytes, b"user42");
}

#[test]
fn test_concat_heap_result_has_headroom() {
    let l = [b'a'; 20];
    let r = [b'b'; 20];
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; arrays readable for 20 bytes each.
    unsafe { ryo_str_concat(&mut slot, l.as_ptr(), 20, r.as_ptr(), 20) };
    assert!(!is_inline(slot.cap));
    assert_eq!(slot.len, 40);
    assert!(slot.cap >= 64, "growth_cap(40) == 64 headroom");
    // SAFETY: heap slot produced above.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
}

#[test]
fn test_concat_empty_left() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; both input buffers are valid for reading.
    unsafe { ryo_str_concat(&mut slot, b"".as_ptr(), 0, b"abc".as_ptr(), 3) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"abc");
}

#[test]
fn test_concat_both_empty() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; len == 0 on both sides, so neither
    // pointer is dereferenced.
    unsafe { ryo_str_concat(&mut slot, core::ptr::null(), 0, core::ptr::null(), 0) };
    assert!(slot.ptr.is_null());
    assert_eq!(slot.len, 0);
    assert_eq!(slot.cap, 0);
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
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_int_to_str(&mut slot, 42) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"42");
}

#[test]
fn test_int_to_str_negative() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_int_to_str(&mut slot, -123) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"-123");
}

#[test]
fn test_int_to_str_zero() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_int_to_str(&mut slot, 0) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"0");
}

#[test]
fn test_int_to_str_min() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_int_to_str(&mut slot, i64::MIN) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"-9223372036854775808");
}

#[test]
fn test_int_to_str_inline() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_int_to_str(&mut slot, -9223372036854775808) }; // 20 chars: max
    assert!(is_inline(slot.cap));
    assert_eq!(inline_len(slot.cap), 20);
    // SAFETY: the inline slot data region holds 20 initialized bytes.
    let bytes = unsafe { core::slice::from_raw_parts(&slot as *const RyoStrFat as *const u8, 20) };
    assert_eq!(bytes, b"-9223372036854775808");
}

#[test]
fn test_float_to_str_nan() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_float_to_str(&mut slot, f64::NAN) };
    assert_eq!(slot_content(&slot), b"nan");
}

#[test]
fn test_float_to_str_inf() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_float_to_str(&mut slot, f64::INFINITY) };
    assert_eq!(slot_content(&slot), b"inf");
}

#[test]
fn test_float_to_str_neg_inf() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_float_to_str(&mut slot, f64::NEG_INFINITY) };
    assert_eq!(slot_content(&slot), b"-inf");
}

#[test]
fn test_float_to_str() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_float_to_str(&mut slot, 2.75) };
    let s = core::str::from_utf8(slot_content(&slot)).unwrap();
    assert!(s.starts_with("2.75"), "got: {}", s);
}

#[test]
fn test_float_to_str_large_value() {
    // Value larger than u64::MAX — old code would saturate
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_float_to_str(&mut slot, 1.8e19) };
    let s = core::str::from_utf8(slot_content(&slot)).unwrap();
    let parsed: f64 = s.parse().unwrap();
    assert_eq!(parsed, 1.8e19);
}

#[test]
fn test_float_to_str_precision() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_float_to_str(&mut slot, 0.1 + 0.2) };
    let s = core::str::from_utf8(slot_content(&slot)).unwrap();
    let parsed: f64 = s.parse().unwrap();
    assert_eq!(parsed, 0.1 + 0.2);
}

#[test]
fn test_bool_to_str_true() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_bool_to_str(&mut slot, 1) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"true");
}

#[test]
fn test_bool_to_str_false() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { ryo_bool_to_str(&mut slot, 0) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"false");
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

        let mut slot = RyoStrFat {
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        ryo_str_concat(
            &mut slot,
            left_fat.ptr,
            left_fat.len,
            right_fat.ptr,
            right_fat.len,
        );

        assert_eq!(slot_content(&slot), b"Hello, World!");

        // Free: static left is safe (cap=0 → noop), heap right freed;
        // the 13-byte inline result needs no free.
        ryo_str_free(left_fat.ptr, left_fat.cap);
        ryo_str_free(right_fat.ptr, right_fat.cap);
    }
}

#[test]
fn str_from_view_copies_bytes() {
    let src = b"hello";
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; src points to 5 readable bytes.
    unsafe { ryo_str_from_view(&mut slot, src.as_ptr(), 5) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"hello");
}

#[test]
fn str_from_view_buffer_is_independent() {
    unsafe {
        // Heap-backed source (> INLINE_CAP so the copy is heap too):
        // the copy must own a fresh buffer.
        let src = ryo_str_alloc(30);
        core::ptr::copy_nonoverlapping(b"abcdefghijklmnopqrstuvwxyzabcd".as_ptr(), src, 30);
        let mut slot = RyoStrFat {
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        // SAFETY: valid out-slot; src is readable for 30 bytes.
        ryo_str_from_view(&mut slot, src, 30);
        assert!(!is_inline(slot.cap));
        assert!(
            !core::ptr::eq(slot.ptr, src),
            "copy must not alias the source"
        );
        // Overwrite and free the source; the copy is unaffected.
        core::ptr::write_bytes(src, b'x', 30);
        ryo_str_free(src, 30);
        assert_eq!(slot_content(&slot), b"abcdefghijklmnopqrstuvwxyzabcd");
        // SAFETY: heap slot produced above; cap is its allocation size.
        ryo_str_free(slot.ptr, slot.cap);
    }
}

#[test]
fn str_from_view_empty() {
    // ptr may be null/dangling when len == 0 (`ryo_str_from_view` invariant).
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; len == 0, so the pointer is never
    // dereferenced.
    unsafe { ryo_str_from_view(&mut slot, core::ptr::null(), 0) };
    assert!(is_inline(slot.cap));
    assert_eq!(inline_len(slot.cap), 0);
}

#[test]
fn test_from_view_inline_and_heap() {
    let mut small = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; source literal readable for 5 bytes.
    unsafe { ryo_str_from_view(&mut small, b"hello".as_ptr(), 5) };
    assert!(is_inline(small.cap));
    let long = [b'y'; 40];
    let mut big = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; `long` readable for 40 bytes.
    unsafe { ryo_str_from_view(&mut big, long.as_ptr(), 40) };
    assert!(!is_inline(big.cap));
    assert_eq!(big.len, 40);
    // SAFETY: heap slot produced above.
    unsafe { ryo_str_free(big.ptr, big.cap) };
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
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; a/b are readable for their lengths.
    unsafe {
        ryo_bytes_concat(
            &mut slot,
            a.as_ptr(),
            a.len() as u64,
            b.as_ptr(),
            b.len() as u64,
        )
    };
    // 3 bytes fits inline (SSO).
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), &[0x01, 0x02, 0x03]);
}

#[test]
fn bytes_concat_empty_is_empty_static() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; len == 0 on both sides, so neither
    // pointer is dereferenced.
    unsafe { ryo_bytes_concat(&mut slot, core::ptr::null(), 0, core::ptr::null(), 0) };
    assert!(slot.ptr.is_null());
    assert_eq!(slot.len, 0);
    assert_eq!(slot.cap, 0);
}

#[test]
fn bytes_from_view_copies() {
    // Small result lands inline.
    let src = [0xaau8, 0xbb];
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; src is readable for 2 bytes.
    unsafe { ryo_bytes_from_view(&mut slot, src.as_ptr(), src.len() as u64) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), &[0xaa, 0xbb]);

    // Large result is an independent heap copy.
    let big = [0xccu8; 30];
    let mut big_slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; `big` is readable for 30 bytes.
    unsafe { ryo_bytes_from_view(&mut big_slot, big.as_ptr(), big.len() as u64) };
    assert!(!is_inline(big_slot.cap));
    assert_ne!(big_slot.ptr, big.as_ptr() as *mut u8); // independent copy
    assert_eq!(slot_content(&big_slot), &big);
    // SAFETY: heap slot produced above; cap is its allocation size.
    unsafe { ryo_bytes_free(big_slot.ptr, big_slot.cap) };
}

#[test]
fn test_push_inline_fits_no_alloc() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { write_str_slot(&mut slot, b"abc") };
    // SAFETY: slot is a valid tagged string; suffix readable for 3 bytes.
    unsafe { __ryo_str_push(&mut slot, b"def".as_ptr(), 3) };
    assert!(is_inline(slot.cap));
    assert_eq!(inline_len(slot.cap), 6);
    // SAFETY: an inline slot holds inline_len initialized bytes in
    // its data region (offsets 0..6).
    let bytes = unsafe { core::slice::from_raw_parts(&slot as *const RyoStrFat as *const u8, 6) };
    assert_eq!(bytes, b"abcdef");
}

#[test]
fn test_push_inline_promotes_on_overflow() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { write_str_slot(&mut slot, b"abcdefghijklmnopqrstuvw") }; // 23
    // SAFETY: slot valid; suffix readable for 1 byte.
    unsafe { __ryo_str_push(&mut slot, b"x".as_ptr(), 1) };
    assert!(!is_inline(slot.cap));
    assert_eq!(slot.len, 24);
    assert!(slot.cap >= 32);
    // SAFETY: heap slot produced above; ptr valid for len bytes.
    let bytes = unsafe { core::slice::from_raw_parts(slot.ptr, 24) };
    assert_eq!(bytes, b"abcdefghijklmnopqrstuvwx");
    // SAFETY: heap slot produced above.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
}

#[test]
fn test_push_static_short_goes_inline() {
    let mut slot = RyoStrFat {
        ptr: b"lit" as *const u8 as *mut u8, // .rodata stand-in
        len: 3,
        cap: 0,
    };
    // SAFETY: slot valid; suffix readable for 2 bytes.
    unsafe { __ryo_str_push(&mut slot, b"!!".as_ptr(), 2) };
    assert!(is_inline(slot.cap));
    assert_eq!(inline_len(slot.cap), 5);
    // SAFETY: an inline slot holds inline_len initialized bytes in
    // its data region (offsets 0..5).
    let bytes = unsafe { core::slice::from_raw_parts(&slot as *const RyoStrFat as *const u8, 5) };
    assert_eq!(bytes, b"lit!!");
}

#[test]
fn test_push_inline_high_len_word_bytes_promote_no_abort() {
    // 23-byte inline bytes value with 0xFF at offsets 8..16: the
    // slot's len word reads as u64::MAX, so a checked_add on it
    // (instead of on the tag's inline_len) would overflow_abort a
    // perfectly legal append.
    let src = [0xffu8; 23];
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; src readable for 23 bytes.
    unsafe { write_str_slot(&mut slot, &src) };
    // SAFETY: slot is a valid tagged string; suffix readable for 1 byte.
    unsafe { __ryo_str_push(&mut slot, b"\x01".as_ptr(), 1) };
    assert!(!is_inline(slot.cap));
    assert_eq!(slot.len, 24);
    assert!(slot.cap >= 32);
    // SAFETY: heap slot produced above; ptr valid for len bytes.
    let bytes = unsafe { core::slice::from_raw_parts(slot.ptr, 24) };
    assert_eq!(&bytes[..23], &[0xffu8; 23]);
    assert_eq!(bytes[23], 0x01);
    // SAFETY: heap slot produced above; cap is its allocation size.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
}

#[test]
fn bytes_push_appends_and_grows_from_static() {
    let src = [0x01u8];
    let mut fat = RyoStrFat {
        ptr: src.as_ptr() as *mut u8, // static cap=0: NOT heap-owned
        len: 1,
        cap: 0,
    };
    // SAFETY: slot valid; byte appended rides __ryo_str_push.
    unsafe { __ryo_bytes_push(&mut fat, 0xff) };
    // Short static append stays off-heap: the result goes inline.
    assert!(is_inline(fat.cap));
    assert_eq!(inline_len(fat.cap), 2);
    assert_eq!(slot_content(&fat), &[0x01, 0xff]);
}

#[test]
fn bytes_push_static_overflow_goes_heap() {
    // Static source whose result exceeds INLINE_CAP still takes the
    // explicit-copy heap path.
    let src = [0x2au8; 30];
    let mut fat = RyoStrFat {
        ptr: src.as_ptr() as *mut u8, // static cap=0: NOT heap-owned
        len: 30,
        cap: 0,
    };
    // SAFETY: slot valid; byte appended rides __ryo_str_push.
    unsafe { __ryo_bytes_push(&mut fat, 0xff) };
    assert!(!is_inline(fat.cap));
    assert_eq!(fat.len, 31);
    assert!(fat.cap >= 31);
    // SAFETY: heap slot produced above; ptr valid for len bytes.
    let s = unsafe { core::slice::from_raw_parts(fat.ptr, fat.len as usize) };
    assert_eq!(&s[..30], &[0x2au8; 30]);
    assert_eq!(s[30], 0xff);
    // SAFETY: heap slot produced above; cap is its allocation size.
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
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; src is readable for its byte length.
    unsafe { __ryo_bytes_to_str(&mut slot, src.as_ptr(), src.len() as u64) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), "héllo".as_bytes());
}

#[test]
fn str_to_bytes_copies() {
    let src = "héllo".as_bytes();
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; src is readable for its byte length.
    unsafe { __ryo_str_to_bytes(&mut slot, src.as_ptr(), src.len() as u64) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), "héllo".as_bytes());
}

#[test]
fn bytes_repr_escapes() {
    // A, NUL, 0xff, newline, '"', '\', '~' (0x7e printable), ESC (0x1b)
    let input = [b'A', 0x00, 0xff, b'\n', b'"', b'\\', 0x7e, 0x1b];
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; input is readable for its byte length.
    unsafe { __ryo_bytes_repr(&mut slot, input.as_ptr(), input.len() as u64) };
    assert_eq!(slot_content(&slot), b"b\"A\\0\\xff\\n\\\"\\\\~\\x1b\"");
    // The verbatim-heap slot reports its real allocation cap, which
    // covers the written length (fixes the old LenIsCap under-report).
    assert!(!is_inline(slot.cap));
    assert!(slot.cap >= slot.len);
    // SAFETY: heap slot produced above; cap is its allocation size.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
}

#[test]
fn bytes_repr_empty() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; len == 0, so the pointer is never
    // dereferenced.
    unsafe { __ryo_bytes_repr(&mut slot, core::ptr::null(), 0) };
    assert_eq!(slot_content(&slot), b"b\"\"");
    assert!(slot.cap >= slot.len);
    // SAFETY: heap slot produced above; cap is its allocation size.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
}

#[test]
fn test_inline_tag_roundtrip() {
    for len in 0..=INLINE_CAP as u64 {
        let cap = inline_tag(len);
        assert!(is_inline(cap));
        assert_eq!(inline_len(cap), len);
    }
    // Heap caps (top byte clear) and the static sentinel are never inline.
    assert!(!is_inline(0));
    assert!(!is_inline(16));
    assert!(!is_inline(u64::MAX >> 8)); // 2^56-1: max legal heap cap
}

#[test]
fn test_write_str_slot_inline() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    let bytes = b"hello ryo sso"; // 13 bytes
    // SAFETY: slot is a valid 24-byte out-slot.
    unsafe { write_str_slot(&mut slot, bytes) };
    assert!(is_inline(slot.cap));
    assert_eq!(inline_len(slot.cap), bytes.len() as u64);
    // Byte content lives in the slot's first `len` bytes.
    // SAFETY: the slot data region holds bytes.len() initialized bytes.
    let stored =
        unsafe { core::slice::from_raw_parts(&slot as *const RyoStrFat as *const u8, bytes.len()) };
    assert_eq!(stored, bytes);
}

#[test]
fn test_write_str_slot_heap_at_boundary() {
    let bytes = [b'x'; INLINE_CAP + 1]; // 24 bytes: one past inline capacity
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: slot is a valid out-slot; result is heap and freed below.
    unsafe { write_str_slot(&mut slot, &bytes) };
    assert!(!is_inline(slot.cap));
    assert_eq!(slot.len, 24);
    assert!(slot.cap >= 24); // headroom allowed, exact fit allowed
    // SAFETY: slot.ptr points to slot.cap (>= 24) initialized bytes.
    let stored = unsafe { core::slice::from_raw_parts(slot.ptr, 24) };
    assert_eq!(stored, &bytes);
    // SAFETY: heap slot produced above; cap is its allocation size.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
}

#[test]
fn test_growth_cap_policy() {
    assert_eq!(growth_cap(1), 16);
    assert_eq!(growth_cap(16), 16);
    assert_eq!(growth_cap(17), 32);
    assert_eq!(growth_cap(1000), 1024);
}

#[test]
fn test_write_str_slot_inline_boundary_sweep() {
    // Every inline length 0..=23, verifying ALL len bytes survive —
    // lengths 17..=23 overlap the cap word's low bytes, which only a
    // byte-23-only tag write preserves.
    for len in 0..=INLINE_CAP {
        let bytes = vec![b'a' + (len % 26) as u8; len];
        let mut slot = RyoStrFat {
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        // SAFETY: slot is a valid 24-byte out-slot.
        unsafe { write_str_slot(&mut slot, &bytes) };
        assert!(is_inline(slot.cap), "len {len} must be inline");
        assert_eq!(inline_len(slot.cap), len as u64);
        // SAFETY: slot data region holds len initialized bytes.
        let stored =
            unsafe { core::slice::from_raw_parts(&slot as *const RyoStrFat as *const u8, len) };
        assert_eq!(stored, &bytes[..], "len {len} content corrupted");
    }
}

#[test]
fn test_ensure_heap_promotes_inline() {
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { write_str_slot(&mut slot, b"slice me please") };
    // SAFETY: slot is a valid tagged RyoStrFat.
    unsafe { __ryo_str_ensure_heap(&mut slot) };
    assert!(!is_inline(slot.cap));
    assert_eq!(slot.len, 15);
    assert!(slot.cap >= 16); // growth headroom
    // SAFETY: slot.ptr points to slot.cap (>= 15) initialized bytes.
    let bytes = unsafe { core::slice::from_raw_parts(slot.ptr, 15) };
    assert_eq!(bytes, b"slice me please");
    // SAFETY: heap slot produced above.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
}

#[test]
fn test_ensure_heap_noop_for_heap_and_static() {
    // Heap: allocated triple passes through untouched.
    let p = ryo_str_alloc(32);
    let mut heap = RyoStrFat {
        ptr: p,
        len: 5,
        cap: 32,
    };
    // SAFETY: heap is a valid tagged slot.
    unsafe { __ryo_str_ensure_heap(&mut heap) };
    assert_eq!(heap.ptr, p);
    assert_eq!(heap.cap, 32);
    // Static: cap == 0 sentinel is not inline — untouched.
    let mut st = RyoStrFat {
        ptr: p,
        len: 5,
        cap: 0,
    };
    // SAFETY: st is a valid tagged slot.
    unsafe { __ryo_str_ensure_heap(&mut st) };
    assert_eq!(st.cap, 0);
    // SAFETY: p came from ryo_str_alloc(32).
    unsafe { ryo_str_free(p, 32) };
}

#[test]
fn test_free_inline_str_is_noop() {
    // An inline slot's ptr word is byte data, NOT a heap pointer;
    // free must no-op on it without dereferencing or calling c_free.
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot.
    unsafe { write_str_slot(&mut slot, b"short") };
    // SAFETY: tagged inline slot; free must recognize the tag.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
    assert!(is_inline(slot.cap)); // slot untouched
}
