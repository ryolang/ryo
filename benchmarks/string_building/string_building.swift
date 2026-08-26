var s = ""
for _ in 0..<50000 {
    s += "x"
}
precondition(s.utf8.count == 50000, "string_building length check")
print("assert passed, string_building is correct")
