var s = "0123456789abcdef"
for _ in 0..<20 {
    s = s + s
}
precondition(s.utf8.count == 16777216, "doubling_concat length check")
print("assert passed, doubling_concat is correct")
