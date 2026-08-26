var total = 0
for i in 0..<500000 {
    let t = String(i) + "!"
    total += t.utf8.count
}
precondition(total == 3388890, "many_small_strings checksum")
print("assert passed, many_small_strings is correct")
