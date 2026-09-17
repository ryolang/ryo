fn count_fox(text: &str) -> usize {
    // String-semantic scan: direct &str slicing validates UTF-8 char
    // boundaries per slice and panics on a split character — the same
    // contract as Ryo's strview slices. The seed is ASCII-only, so the
    // checks always pass here, but both languages pay them per window.
    let n = text.len();
    (0..n.saturating_sub(2))
        .filter(|&i| &text[i..i + 3] == "fox")
        .count()
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
