struct Person {
    var name: String
    var age: Int
}

func makePerson(_ i: Int) -> Person {
    Person(name: "user" + String(i), age: 20 + i % 50)
}

func birthday(_ p: Person) -> Person {
    Person(name: p.name, age: p.age + 1)
}

func score(_ p: Person) -> Int {
    p.name.utf8.count + p.age
}

var total = 0
for i in 0..<500000 {
    let p = makePerson(i)
    let q = birthday(p)
    total += score(p) + score(q)
}
precondition(total == 54777780, "struct_records_reuse checksum")
print("assert passed, struct_records_reuse is correct")
