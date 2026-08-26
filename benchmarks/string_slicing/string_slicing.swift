import Foundation

func countFox(_ text: [UInt8]) -> Int {
	var count = 0
	var i = 0
	let n = text.count
	while i + 3 <= n {
		if text[i] == 102 && text[i + 1] == 111 && text[i + 2] == 120 {
			count += 1
		}
		i += 1
	}
	return count
}

var s = "the quick brown fox jumps over the lazy dog"
for _ in 0..<14 {
	s = s + s
}
let bytes = [UInt8](s.utf8)
let count = countFox(bytes)
let n = bytes.count
precondition(n == 704512, "string_slicing length check")
precondition(count == 16384, "string_slicing match count check")
print("assert passed, string_slicing is correct")
