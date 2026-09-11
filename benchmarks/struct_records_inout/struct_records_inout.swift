struct Person {
    var name: String
    var age: Int
}

func makePerson(_ i: Int) -> Person {
    Person(name: "user" + String(i), age: 20 + i % 50)
}

func birthday(_ p: inout Person) {
    p.age += 1
}

func score(_ p: Person) -> Int {
    p.name.utf8.count + p.age
}

var total = 0
for i in 0..<500000 {
    var p = makePerson(i)
    birthday(&p)
    total += score(p)
}
precondition(total == 27638890, "struct_records_inout checksum")
print("assert passed, struct_records_inout is correct")
