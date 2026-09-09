struct Person {
    var name: String
    var age: Int
}

func makePerson(_ i: Int) -> Person {
    Person(name: "user" + String(i), age: 20 + i % 50)
}

func birthday(_ p: Person) -> Person {
    Person(name: "user" + String(p.age), age: p.age + 1)
}

func score(_ p: Person) -> Int {
    p.name.utf8.count + p.age
}

var total = 0
for i in 0..<500000 {
    let p = makePerson(i)
    let q = birthday(p)
    total += score(q)
}
precondition(total == 25750000, "struct_records checksum")
print("assert passed, struct_records is correct")
