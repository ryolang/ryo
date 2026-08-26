fn count_fox(text: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    let n = text.len();
    while i + 3 <= n {
        if &text[i..i + 3] == b"fox" {
            count += 1;
        }
        i += 1;
    }
    count
}

fn main() {
    let mut s = String::from("the quick brown fox jumps over the lazy dog");
    for _ in 0..14 {
        s = s.clone() + &s;
    }
    let count = count_fox(s.as_bytes());
    let n = s.len();
    assert_eq!(n, 704512, "string_slicing length check");
    assert_eq!(count, 16384, "string_slicing match count check");
    println!("assert passed, string_slicing is correct");
}
