fn main() {
    let mut s = "0123456789abcdef".to_string();
    for _ in 0..20 {
        s = format!("{s}{s}");
    }
    assert!(s.len() == 16777216, "doubling_concat length check");
    println!("assert passed, doubling_concat is correct");
}
