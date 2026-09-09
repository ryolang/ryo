struct Person {
    name: String,
    age: i64,
}

fn make_person(i: i64) -> Person {
    Person {
        name: format!("user{}", i),
        age: 20 + i % 50,
    }
}

fn birthday(p: Person) -> Person {
    Person {
        name: format!("user{}", p.age),
        age: p.age + 1,
    }
}

fn score(p: &Person) -> i64 {
    p.name.len() as i64 + p.age
}

fn main() {
    let mut total = 0i64;
    for i in 0..500000 {
        let p = make_person(i);
        let q = birthday(p);
        total += score(&q);
    }
    assert!(total == 25750000, "struct_records checksum");
    println!("assert passed, struct_records is correct");
}
