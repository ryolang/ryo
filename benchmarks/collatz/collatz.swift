import Foundation

func collatzSteps(_ start: Int) -> Int {
	var n = start
	var steps = 0
	while n != 1 {
		if n % 2 == 0 {
			n = n / 2
		} else {
			n = 3 * n + 1
		}
		steps += 1
	}
	return steps
}

var total = 0
for i in 1...1_000_000 {
	total += collatzSteps(i)
}
precondition(total == 131434424, "collatz checksum")
print("assert passed, collatz is correct")
