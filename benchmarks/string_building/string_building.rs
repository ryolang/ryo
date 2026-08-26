fn main() {
    let mut s = String::new();
    for _ in 0..50000 {
        s.push_str("x");
    }
    assert!(s.len() == 50000, "string_building length check");
    println!("assert passed, string_building is correct");
}
