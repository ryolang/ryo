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

fn birthday(p: &mut Person) {
    p.age += 1;
}

fn score(p: &Person) -> i64 {
    p.name.len() as i64 + p.age
}

fn main() {
    let mut total = 0i64;
    for i in 0..500000 {
        let mut p = make_person(i);
        birthday(&mut p);
        total += score(&p);
    }
    assert!(total == 27638890, "struct_records_inout checksum");
    println!("assert passed, struct_records_inout is correct");
}
