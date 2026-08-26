fn collatz_steps(start: i64) -> i64 {
    let mut n = start;
    let mut steps: i64 = 0;
    while n != 1 {
        if n % 2 == 0 {
            n /= 2;
        } else {
            n = 3 * n + 1;
        }
        steps += 1;
    }
    steps
}

fn main() {
    let mut total: i64 = 0;
    for i in 1..1_000_001_i64 {
        total += collatz_steps(i);
    }
    assert_eq!(total, 131434424, "collatz checksum");
    println!("assert passed, collatz is correct");
}
