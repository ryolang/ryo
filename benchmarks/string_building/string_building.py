s = ""
for _ in range(50000):
    s += "x"
assert len(s) == 50000, "string_building length check"
print("assert passed, string_building is correct")
