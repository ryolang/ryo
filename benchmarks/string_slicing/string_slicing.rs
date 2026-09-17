fn count_fox(text: &str) -> usize {
    let n = text.len();
    let mut count = 0;
    let mut i = 0;
    while i + 3 <= n {
        // Byte-offset slicing with UTF-8 char-boundary validation, the
        // same semantics as Ryo's strview slices: direct slicing panics
        // when the range splits a character (Ryo panics with exit 101).
        // The seed is ASCII-only, so the checks always pass here — but
        // both languages pay them per iteration.
        if &text[i..i + 3] == "fox" {
            count += 1;
        }
        i += 1;
    }
    count
}

fn main() {
    let mut s = String::from("the quick brown fox jumps over the lazy dog");
    for _ in 0..14 {
        s = s.repeat(2);
    }
    let count = count_fox(&s);
    let n = s.len();
    assert_eq!(n, 704512, "string_slicing length check");
    assert_eq!(count, 16384, "string_slicing match count check");
    println!("assert passed, string_slicing is correct");
}
