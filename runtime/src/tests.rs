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

/// Combined argv test (M9.2): ARGC/ARGV are the runtime's first
/// process-wide mutable globals, so every argv test lives in this ONE
/// test fn — parallel cargo-test threads would otherwise race on them.
///
/// Besides the storage roundtrip (below), this fn guards the
/// publication contract of those globals: a reader thread spins on
/// ARGC (Acquire) and dereferences ARGV (Acquire) while a writer
/// thread publishes through `ryo_rt_init` (data-then-flag Release).
/// Normal runs only exercise the threads — the race is undetectable
/// without weak-memory emulation — but under Miri each
/// `MIRIFLAGS=-Zmiri-seed` explores different interleavings, and the
/// test fails if the publish order is ever inverted or the Acquire
/// pairing breaks. The Miri CI job sweeps a small seed range.
///
/// The out-of-range panic path terminates the process, so it is
/// asserted via a subprocess: this test binary re-executes itself with
/// `RYO_RT_ARGV_PANIC_CHILD=1`, and the flag gates the diverging call
/// at the top of THIS test. The child never runs its init, so ARGC is
/// 0 and the read is trivially out of range; the parent asserts the
/// child's exit code 101 and stderr message.
#[test]
fn argv_storage_roundtrip() {
    use alloc::ffi::CString;

    // Child mode: diverge before the roundtrip below.
    if std::env::var_os("RYO_RT_ARGV_PANIC_CHILD").is_some() {
        // The parent parameterizes the failing index so both the
        // too-large and the negative paths get their own child run.
        let index: i64 = std::env::var("RYO_RT_ARGV_PANIC_INDEX")
            .expect("child index")
            .parse()
            .expect("child index parses");
        let mut slot = RyoStrFat {
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        // SAFETY: valid out-slot. ryo_rt_init never ran in the child,
        // so ARGC is 0 and the index is trivially out of range — this
        // must write the panic message to stderr and exit 101, never
        // returning.
        unsafe { ryo_process_argv(index, &mut slot) };
        panic!("ryo_process_argv({index}) returned; out-of-range must diverge");
    }

    // Publication-contract guard: reset the globals, then race a
    // reader against ryo_rt_init's ARGV-then-ARGC stores.
    ARGC.store(0, core::sync::atomic::Ordering::Release);
    ARGV.store(core::ptr::null_mut(), core::sync::atomic::Ordering::Release);
    let probe = CString::new("probe").expect("CString");
    let probe_argv = [probe.as_ptr()];
    // Raw pointers are not Send; the address as usize is. Provenance is
    // re-established by the cast inside the writer, where the SAFETY
    // comment covers it.
    let probe_argv_addr = probe_argv.as_ptr() as usize;
    let reader = std::thread::spawn(move || {
        loop {
            if ARGC.load(core::sync::atomic::Ordering::Acquire) == 1 {
                // Acquire pairs with the writer's Release: passing the ARGC
                // check must force this load to see the stored pointer.
                // ARGV holds the C argv table (a char** stored through an
                // AtomicPtr<c_char>, same as ryo_process_argv reads it).
                let table = ARGV.load(core::sync::atomic::Ordering::Acquire)
                    as *const *const core::ffi::c_char;
                // SAFETY: the contract under test — once ARGC is 1, `table`
                // must be the pointer ryo_rt_init stored, never stale/null.
                // Miri flags the dereference if the ordering ever lets a
                // stale table through.
                let s = unsafe { *table };
                // SAFETY: s is the NUL-terminated C string published via
                // ryo_rt_init; `probe` outlives the join below.
                return unsafe { *s };
            }
            std::thread::yield_now();
        }
    });
    let writer = std::thread::spawn(move || {
        // SAFETY: the address came from `probe_argv`, a 1-element table
        // of a readable NUL-terminated C string (`probe`) that outlives
        // the call (both threads are joined before `probe` drops).
        let argv = probe_argv_addr as *const *const core::ffi::c_char;
        unsafe { ryo_rt_init(1, argv) };
    });
    writer.join().expect("publication writer");
    let first = reader.join().expect("publication reader");
    assert_eq!(first as u8, b'p');

    let a = CString::new("a").expect("CString");
    let b = CString::new("b").expect("CString");
    let c = CString::new("c").expect("CString");
    let argv = [a.as_ptr(), b.as_ptr(), c.as_ptr()];
    // SAFETY: argv points to 3 readable NUL-terminated C strings; the
    // CStrings outlive every ryo_process_argv call below (they model
    // the C runtime's process-lifetime ownership of the real argv).
    unsafe { ryo_rt_init(3, argv.as_ptr()) };

    // SAFETY: no other test fn touches these atomics (see fn doc).
    assert_eq!(unsafe { ryo_process_argc() }, 3);

    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    for (i, want) in [(0i64, &b"a"[..]), (1, &b"b"[..]), (2, &b"c"[..])] {
        // SAFETY: valid out-slot; i < argc (3), in range.
        unsafe { ryo_process_argv(i, &mut slot) };
        assert!(is_inline(slot.cap));
        assert_eq!(slot_content(&slot), want);
    }

    // Out-of-range: the re-exec'd child terminates with the panic
    // message and exit 101 — run once for a too-large index and once
    // for a negative one (the Ryo index is a signed int). Skipped
    // under Miri: its isolation mode cannot spawn processes. The
    // in-process roundtrip above still runs under Miri, so the new
    // unsafe read path keeps its UB/leak coverage.
    if !cfg!(miri) {
        let run_child = |index: &str| {
            let out = std::process::Command::new(std::env::current_exe().expect("current_exe"))
                .arg("tests::argv_storage_roundtrip")
                .arg("--exact")
                .env("RYO_RT_ARGV_PANIC_CHILD", "1")
                .env("RYO_RT_ARGV_PANIC_INDEX", index)
                .output()
                .expect("spawn argv panic child");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                out.status.code(),
                Some(101),
                "out-of-range process_argv({index}) must exit 101. stderr: {stderr}"
            );
            assert!(
                stderr.contains("process_argv index out of range"),
                "child stderr should carry the panic message, got: {stderr}"
            );
            stderr.into_owned()
        };
        let stderr = run_child("3");
        assert!(
            stderr.contains(": 3"),
            "child stderr should name the failing index 3, got: {stderr}"
        );
        let stderr = run_child("-1");
        assert!(
            stderr.contains(": -1"),
            "child stderr should name the failing index -1, got: {stderr}"
        );
    }
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

// ---------- io_read_line (M9.2) ----------

/// Write `content` to a fresh temp file and rewind it. Returns the open
/// file (its fd readable from the start) and the path for cleanup.
/// `tempfile` is intentionally not a dev-dependency: a pid + counter
/// name in the system temp dir is enough, and Miri's isolated FS
/// already permits TMPDIR.
///
/// Windows is excluded: `_read` needs a CRT file descriptor, which a
/// std `File` does not expose (`as_raw_handle` is a HANDLE).
#[cfg(not(windows))]
fn temp_file_with(content: &[u8]) -> (std::fs::File, std::path::PathBuf) {
    use std::io::{Seek, SeekFrom, Write};
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "ryo_rt_read_line_{}_{}.tmp",
        std::process::id(),
        N.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
    ));
    // OpenOptions, not File::create: the fd must be readable —
    // File::create is write-only (O_WRONLY) and read would fail EBADF.
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create temp file");
    f.write_all(content).expect("write temp file");
    f.seek(SeekFrom::Start(0)).expect("rewind temp file");
    (f, path)
}

/// A line terminated by `\n` comes back without it — and the read
/// stops at the newline, leaving later bytes unread.
#[cfg(not(windows))]
#[test]
fn read_line_strips_trailing_newline() {
    // Miri's isolation mode forbids `open` (temp_file_with), so the
    // fd-parameterized path is verified by the normal runs only; the
    // in-process read logic itself has no unsafe isolation conflicts.
    if cfg!(miri) {
        return;
    }
    use std::os::unix::io::AsRawFd;

    let (file, path) = temp_file_with(b"hello\nrest");
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; the file's fd is open and readable.
    unsafe { read_line_from(file.as_raw_fd(), &mut slot) };
    assert!(is_inline(slot.cap));
    assert_eq!(slot_content(&slot), b"hello");
    drop(file);
    std::fs::remove_file(&path).expect("remove temp file");
}

/// EOF after partial bytes returns the final unterminated line as-is.
/// 300 bytes with no '\n' forces several `read` calls and buffer
/// growth past the 128-byte initial cap — the result must still come
/// back complete (heap slot, freed here).
#[cfg(not(windows))]
#[test]
fn read_line_unterminated_final_line_is_returned_as_is() {
    // See read_line_strips_trailing_newline: Miri isolation forbids
    // the temp-file open.
    if cfg!(miri) {
        return;
    }
    use std::os::unix::io::AsRawFd;

    let content = [b'x'; 300];
    let (file, path) = temp_file_with(&content);
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; the file's fd is open and readable.
    unsafe { read_line_from(file.as_raw_fd(), &mut slot) };
    assert!(!is_inline(slot.cap));
    assert_eq!(slot.len, 300);
    assert_eq!(slot_content(&slot), &content);
    // SAFETY: heap slot produced above; cap is its allocation size.
    unsafe { ryo_str_free(slot.ptr, slot.cap) };
    drop(file);
    std::fs::remove_file(&path).expect("remove temp file");
}

/// EOF before any byte yields the empty slot — the io_read_line
/// contract maps EOF to "", not to an error.
#[cfg(not(windows))]
#[test]
fn read_line_empty_file_yields_empty_slot() {
    // See read_line_strips_trailing_newline: Miri isolation forbids
    // the temp-file open (and even reads from stdin — only stdout /
    // stderr writes are permitted), so this path has no Miri coverage;
    // ASan/Valgrind and the normal runs carry it.
    if cfg!(miri) {
        return;
    }
    use std::os::unix::io::AsRawFd;

    let (file, path) = temp_file_with(b"");
    let mut slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: valid out-slot; the file's fd is open at EOF, so the
    // first read returns 0.
    unsafe { read_line_from(file.as_raw_fd(), &mut slot) };
    assert!(is_inline(slot.cap));
    assert_eq!(inline_len(slot.cap), 0);
    assert_eq!(slot_content(&slot), b"");
    drop(file);
    std::fs::remove_file(&path).expect("remove temp file");
}

/// Combined env test (M9.2): `std::env::set_var`/`remove_var` mutate the
/// process-wide environment, which parallel cargo-test threads could
/// observe mid-mutation, so every ryo_getenv case lives in this ONE test
/// fn.
///
/// The over-long-key panic path terminates the process, so it is
/// asserted via a subprocess: this test binary re-executes itself with
/// `RYO_RT_ENV_PANIC_CHILD=1`, and the flag gates the diverging call at
/// the top of THIS test. The parent asserts the child's exit code 101
/// and stderr message.
#[test]
fn getenv_present_unset_and_long_key() {
    use alloc::ffi::CString;

    // Child mode: diverge before the cases below.
    if std::env::var_os("RYO_RT_ENV_PANIC_CHILD").is_some() {
        let long_key = [b'k'; 4097]; // one past the 4096-byte cap
        let mut slot = RyoStrFat {
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        // SAFETY: valid out-slot; long_key is readable for 4097 bytes —
        // past the cap, so this must write the panic message to stderr
        // and exit 101, never returning.
        unsafe { ryo_getenv(long_key.as_ptr(), long_key.len() as u64, &mut slot) };
        panic!("ryo_getenv with a 4097-byte key returned; over-long key must diverge");
    }

    // Present key: skipped on Windows — std::env::set_var writes via
    // SetEnvironmentVariableW, which the CRT's getenv snapshot (taken
    // at process start) never sees; the documented narrow-getenv
    // placeholder limitation (M16's process.env switches to the wide
    // API). The integration test covers the real path there via env
    // inherited at spawn. Unix setenv updates the CRT environ, so the
    // present case runs in-process.
    if !cfg!(windows) {
        const PRESENT: &str = "RYO_RT_GETENV_PRESENT";
        const VALUE: &str = "ryo-env-value";
        // SAFETY: the combined-fn rule keeps every env mutation in this
        // binary in this one thread, and no other test reads these
        // sentinel keys.
        unsafe { std::env::set_var(PRESENT, VALUE) };

        let mut slot = RyoStrFat {
            ptr: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        let key = CString::new(PRESENT).expect("CString");
        // SAFETY: key's bytes are readable for their length (a C string is
        // readable up to and including its NUL); slot is a valid out-slot.
        // The variable is set above.
        unsafe {
            ryo_getenv(
                key.as_bytes().as_ptr(),
                key.as_bytes().len() as u64,
                &mut slot,
            )
        };
        assert_eq!(slot_content(&slot), VALUE.as_bytes());

        // SAFETY: same single-thread argument as set_var above.
        unsafe { std::env::remove_var(PRESENT) };
    }

    // Unset variable: the M16 placeholder contract — empty string,
    // never a dangling or garbage slot.
    let unset = CString::new("RYO_RT_GETENV_DEFINITELY_UNSET").expect("CString");
    let mut unset_slot = RyoStrFat {
        ptr: core::ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    // SAFETY: same contracts; the variable does not exist, so the slot
    // must come back empty.
    unsafe {
        ryo_getenv(
            unset.as_bytes().as_ptr(),
            unset.as_bytes().len() as u64,
            &mut unset_slot,
        )
    };
    assert_eq!(slot_content(&unset_slot), b"");

    // Over-long key: the re-exec'd child terminates with the panic
    // message and exit 101. Skipped under Miri: its isolation mode
    // cannot spawn processes. The in-process cases above still run
    // under Miri, so the new unsafe read path keeps its
    // UB/leak coverage.
    if !cfg!(miri) {
        let out = std::process::Command::new(std::env::current_exe().expect("current_exe"))
            .arg("tests::getenv_present_unset_and_long_key")
            .arg("--exact")
            .env("RYO_RT_ENV_PANIC_CHILD", "1")
            .output()
            .expect("spawn env panic child");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(101),
            "over-long process_env key must exit 101. stderr: {stderr}"
        );
        assert!(
            stderr.contains("process_env key too long"),
            "child stderr should carry the panic message, got: {stderr}"
        );
    }
}

#[test]
fn stack_limit_from_reserves_margin_below_base() {
    let base = 0x7fff_0000_0000usize;
    let limit = stack_limit_from(base, 8 * 1024 * 1024);
    assert!(limit < base);
    assert_eq!(base - limit, 8 * 1024 * 1024 - 32 * 1024);
}

#[test]
fn stack_limit_from_caps_oversized_stack() {
    // A huge-but-finite rlimit (e.g. RLIM_SAVED_CUR) must not push the
    // limit below every reachable SP — that would disable the guard.
    let base = 0x7fff_0000_0000usize;
    let limit = stack_limit_from(base, MAX_RECORDED_STACK_SIZE);
    assert!(limit < base);
    assert_eq!(
        stack_limit_from(base, MAX_RECORDED_STACK_SIZE * 2),
        limit,
        "sizes above the cap must be clamped to it"
    );
}

#[cfg(not(windows))]
#[test]
fn stack_size_from_rlim_cur_infinity_encodings_fall_back() {
    // Linux RLIM_INFINITY.
    assert_eq!(stack_size_from_rlim_cur(u64::MAX), 8 * 1024 * 1024);
    // Darwin RLIM_INFINITY (2^63 - 1).
    assert_eq!(
        stack_size_from_rlim_cur(0x7fff_ffff_ffff_ffff),
        8 * 1024 * 1024
    );
}

#[cfg(not(windows))]
#[test]
fn stack_size_from_rlim_cur_finite_values_pass_through() {
    assert_eq!(stack_size_from_rlim_cur(8 * 1024 * 1024), 8 * 1024 * 1024);
    // RLIM_SAVED_CUR-style value (Darwin: RLIM_INFINITY - 1) is a
    // huge-but-finite limit, not infinity — it must pass through.
    assert_eq!(
        stack_size_from_rlim_cur(0x7fff_ffff_ffff_fffe),
        0x7fff_ffff_ffff_fffe
    );
}
