import Foundation

let fox = Array("fox".utf8)

func countFox(_ text: [UInt8]) -> Int {
	var count = 0
	var i = 0
	let n = text.count
	while i + 3 <= n {
		if text[i..<(i + 3)].elementsEqual(fox) {
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
precondition(n == 704512, "byte_slicing length check")
precondition(count == 16384, "byte_slicing match count check")
print("assert passed, byte_slicing is correct")
