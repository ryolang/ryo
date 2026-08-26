import Foundation

func escapes(_ cr: Double, _ ci: Double) -> Int {
	var zr: Double = 0.0
	var zi: Double = 0.0
	var i = 0
	while i < 80 {
		let zr2 = zr * zr
		let zi2 = zi * zi
		if zr2 + zi2 > 4.0 {
			return i
		}
		zi = 2.0 * zr * zi + ci
		zr = zr2 - zi2 + cr
		i += 1
	}
	return i
}

var total = 0
var ci: Double = -1.0
while ci <= 1.0 {
	var cr: Double = -2.0
	while cr <= 0.5 {
		total += escapes(cr, ci)
		cr += 0.005
	}
	ci += 0.005
}
precondition(total == 5926720, "mandelbrot checksum")
print("assert passed, mandelbrot is correct")
