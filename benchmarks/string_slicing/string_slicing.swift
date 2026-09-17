import Foundation

let fox = Array("fox".utf8)

// String-semantic scan: iterate the string's native UTF-8 storage by
// index and compare 3-byte UTF-8 view slices — no [UInt8]
// materialization, no raw byte buffer.
func countFox(_ text: String) -> Int {
	let utf8 = text.utf8
	var count = 0
	var i = utf8.startIndex
	while let end = utf8.index(i, offsetBy: 3, limitedBy: utf8.endIndex) {
		// UTF-8 char-boundary validation, mirroring Ryo's strview
		// slice contract: both endpoints must not land on a
		// continuation byte (top bits 10).
		let startOK = (utf8[i] & 0xC0) != 0x80
		let endOK = end == utf8.endIndex || (utf8[end] & 0xC0) != 0x80
		if startOK && endOK && utf8[i..<end].elementsEqual(fox) {
			count += 1
		}
		i = utf8.index(after: i)
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
