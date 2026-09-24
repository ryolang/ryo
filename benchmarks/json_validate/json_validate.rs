// json_validate — recursive-descent JSON validator over a byte slice.
// Byte-identical algorithm to json_validate.ryo / json_validate.py:
// every parse function returns the next position after the consumed
// token, or -1 on error.

fn skip_ws(b: &[u8], p: usize) -> i64 {
    let mut i = p;
    while i < b.len() {
        let c = b[i];
        if c == 32 || c == 9 || c == 10 || c == 13 {
            i += 1;
        } else {
            return i as i64;
        }
    }
    i as i64
}

fn is_hex_digit(c: u8) -> bool {
    if (48..=57).contains(&c) {
        return true;
    }
    if (65..=70).contains(&c) {
        return true;
    }
    if (97..=102).contains(&c) {
        return true;
    }
    false
}

fn parse_string(b: &[u8], p: usize) -> i64 {
    if p >= b.len() || b[p] != 34 {
        return -1;
    }
    let mut i = p + 1;
    while i < b.len() {
        let c = b[i];
        if c == 34 {
            return (i + 1) as i64;
        } else if c == 92 {
            if i + 1 >= b.len() {
                return -1;
            }
            let e = b[i + 1];
            if e == 34 || e == 92 || e == 47 || e == 98 || e == 102 || e == 110 || e == 114 || e == 116 {
                i += 2;
            } else if e == 117 {
                if i + 6 > b.len() {
                    return -1;
                }
                let mut ok = true;
                for k in 1..5 {
                    if !is_hex_digit(b[i + 1 + k]) {
                        ok = false;
                    }
                }
                if !ok {
                    return -1;
                }
                i += 6;
            } else {
                return -1;
            }
        } else if c < 32 {
            return -1;
        } else {
            i += 1;
        }
    }
    -1
}

fn parse_number(b: &[u8], p: usize) -> i64 {
    let mut i = p;
    let n = b.len();
    if i < n && b[i] == 45 {
        i += 1;
    }
    if i >= n {
        return -1;
    }
    if b[i] == 48 {
        i += 1;
    } else if (49..=57).contains(&b[i]) {
        while i < n && (48..=57).contains(&b[i]) {
            i += 1;
        }
    } else {
        return -1;
    }
    if i < n && b[i] == 46 {
        i += 1;
        if i >= n || !(48..=57).contains(&b[i]) {
            return -1;
        }
        while i < n && (48..=57).contains(&b[i]) {
            i += 1;
        }
    }
    if i < n && (b[i] == 101 || b[i] == 69) {
        i += 1;
        if i < n && (b[i] == 43 || b[i] == 45) {
            i += 1;
        }
        if i >= n || !(48..=57).contains(&b[i]) {
            return -1;
        }
        while i < n && (48..=57).contains(&b[i]) {
            i += 1;
        }
    }
    i as i64
}

fn match_lit(b: &[u8], p: usize, lit: &[u8]) -> i64 {
    let n = lit.len();
    if p + n > b.len() {
        return -1;
    }
    for k in 0..n {
        if b[p + k] != lit[k] {
            return -1;
        }
    }
    (p + n) as i64
}

fn parse_value(b: &[u8], p: usize) -> i64 {
    let i = skip_ws(b, p) as usize;
    if i >= b.len() {
        return -1;
    }
    let c = b[i];
    if c == 123 {
        return parse_object(b, i);
    }
    if c == 91 {
        return parse_array(b, i);
    }
    if c == 34 {
        return parse_string(b, i);
    }
    if c == 116 {
        return match_lit(b, i, b"true");
    }
    if c == 102 {
        return match_lit(b, i, b"false");
    }
    if c == 110 {
        return match_lit(b, i, b"null");
    }
    if c == 45 || (48..=57).contains(&c) {
        return parse_number(b, i);
    }
    -1
}

fn parse_array(b: &[u8], p: usize) -> i64 {
    let mut i = skip_ws(b, p + 1) as usize;
    if i < b.len() && b[i] == 93 {
        return (i + 1) as i64;
    }
    let r = parse_value(b, i);
    if r < 0 {
        return -1;
    }
    i = r as usize;
    loop {
        i = skip_ws(b, i) as usize;
        if i >= b.len() {
            return -1;
        }
        let c = b[i];
        if c == 44 {
            let r = parse_value(b, i + 1);
            if r < 0 {
                return -1;
            }
            i = r as usize;
        } else if c == 93 {
            return (i + 1) as i64;
        } else {
            return -1;
        }
    }
}

fn parse_object(b: &[u8], p: usize) -> i64 {
    let mut i = skip_ws(b, p + 1) as usize;
    if i < b.len() && b[i] == 125 {
        return (i + 1) as i64;
    }
    let r = parse_string(b, i);
    if r < 0 {
        return -1;
    }
    i = skip_ws(b, r as usize) as usize;
    if i >= b.len() || b[i] != 58 {
        return -1;
    }
    let r = parse_value(b, i + 1);
    if r < 0 {
        return -1;
    }
    i = r as usize;
    loop {
        i = skip_ws(b, i) as usize;
        if i >= b.len() {
            return -1;
        }
        let c = b[i];
        if c == 44 {
            i = skip_ws(b, i + 1) as usize;
            let r = parse_string(b, i);
            if r < 0 {
                return -1;
            }
            i = skip_ws(b, r as usize) as usize;
            if i >= b.len() || b[i] != 58 {
                return -1;
            }
            let r = parse_value(b, i + 1);
            if r < 0 {
                return -1;
            }
            i = r as usize;
        } else if c == 125 {
            return (i + 1) as i64;
        } else {
            return -1;
        }
    }
}

fn validate(b: &[u8]) -> bool {
    let p = parse_value(b, 0);
    if p < 0 {
        return false;
    }
    skip_ws(b, p as usize) == b.len() as i64
}

fn check_valid(s: &str) {
    assert!(validate(s.as_bytes()), "expected valid");
}

fn check_invalid(s: &str) {
    assert!(!validate(s.as_bytes()), "expected invalid");
}

fn main() {
    check_valid("{}");
    check_valid("[]");
    check_valid(" {\"a\": [1, 2.5e-3, true, null], \"b\": \"x\"} ");
    check_valid("{\"esc\": \"a\\\"b\\\\c\\/\\b\\f\\n\\r\\t\"}");
    check_valid("\"A\u{e9}\"");
    check_valid("[0, -1, 3.14, 1e10, 2.5E-3, -0.5e+2]");
    check_valid("{\"a\": {\"b\": {\"c\": [true, [false, [null]]]}}}");
    check_valid("123");

    check_invalid("");
    check_invalid(" ");
    check_invalid("{\"a\":}");
    check_invalid("[1,]");
    check_invalid("{\"a\" 1}");
    check_invalid("01");
    check_invalid("\"abc");
    check_invalid("{\"a\": 1}]");
    check_invalid("\"\\x\"");
    check_invalid("[1 2]");
    check_invalid("tru");
    check_invalid("nulll");
    check_invalid("{a: 1}");
    check_invalid("\"a\tb\"");
    check_invalid("1.");
    check_invalid("1e");
    check_invalid("-");

    let mut s = String::from("[");
    for i in 0..30000 {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"id\": ");
        s.push_str(&i.to_string());
        s.push_str(", \"name\": \"user");
        s.push_str(&i.to_string());
        s.push_str("\", \"tags\": [\"a\", \"b\"], \"active\": true, \"score\": 1.5, \"misc\": null}");
    }
    s.push(']');

    let raw = s.into_bytes();

    let mut ok = true;
    for _ in 0..12 {
        if !validate(&raw) {
            ok = false;
        }
    }
    assert!(ok, "json_validate big document");
    println!("assert passed, json_validate is correct");
}
