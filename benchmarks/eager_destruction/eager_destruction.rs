fn recursive(x: i64) {
    if x == 0 {
        return;
    }
    let s = x.to_string();
    if s.len() == 0 {
        return;
    }
    recursive(x - 1);
}

fn main() {
    recursive(50000);
}
