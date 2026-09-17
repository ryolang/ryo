fn count_fox(text: &str) -> usize {
    let n = text.len();
    let mut count = 0;
    let mut i = 0;
    while i + 3 <= n {
        // Byte-offset slicing with UTF-8 char-boundary validation, the
        // same semantics as Ryo's strview slices: `get` returns None
        // (rather than panicking) when the range splits a character.
        if text.get(i..i + 3) == Some("fox") {
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
