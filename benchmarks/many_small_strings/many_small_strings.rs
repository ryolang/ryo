fn main() {
    let mut total = 0usize;
    for i in 0..500000 {
        let t = i.to_string() + "!";
        total += t.len();
    }
    assert!(total == 3388890, "many_small_strings checksum");
    println!("assert passed, many_small_strings is correct");
}
