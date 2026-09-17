import Foundation

// String-semantic scan: walk the string Character by Character and
// compare 3-Character Substring windows against the needle. Boundary
// correctness is structural here — String.Index can never split a
// Character, so no explicit validation exists or is needed. (For this
// ASCII-only input, Character windows coincide with Ryo's 3-byte
// windows; on non-ASCII input the semantics diverge — see README.)
func countFox(_ text: String) -> Int {
	var count = 0
	var i = text.startIndex
	while let end = text.index(i, offsetBy: 3, limitedBy: text.endIndex) {
		if text[i..<end] == "fox" {
			count += 1
		}
		i = text.index(after: i)
	}
	return count
}

var s = "the quick brown fox jumps over the lazy dog"
for _ in 0..<14 {
	s = s + s
}
let count = countFox(s)
let n = s.utf8.count
precondition(n == 704512, "string_slicing length check")
precondition(count == 16384, "string_slicing match count check")
print("assert passed, string_slicing is correct")
