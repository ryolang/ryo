fn escapes(cr: f64, ci: f64) -> i64 {
    let mut zr: f64 = 0.0;
    let mut zi: f64 = 0.0;
    let mut i: i64 = 0;
    while i < 80 {
        let zr2 = zr * zr;
        let zi2 = zi * zi;
        if zr2 + zi2 > 4.0 {
            return i;
        }
        zi = 2.0 * zr * zi + ci;
        zr = zr2 - zi2 + cr;
        i += 1;
    }
    i
}

fn main() {
    let mut total: i64 = 0;
    let mut ci: f64 = -1.0;
    while ci <= 1.0 {
        let mut cr: f64 = -2.0;
        while cr <= 0.5 {
            total += escapes(cr, ci);
            cr += 0.005;
        }
        ci += 0.005;
    }
    assert_eq!(total, 5926720, "mandelbrot checksum");
    println!("assert passed, mandelbrot is correct");
}
