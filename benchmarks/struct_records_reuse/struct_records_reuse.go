package main

import "fmt"

type Person struct {
	Name string
	Age  int
}

func makePerson(i int) Person {
	return Person{Name: fmt.Sprintf("user%d", i), Age: 20 + i%50}
}

func birthday(p Person) Person {
	return Person{Name: p.Name, Age: p.Age + 1}
}

func score(p Person) int {
	return len(p.Name) + p.Age
}

func main() {
	total := 0
	for i := 0; i < 500000; i++ {
		p := makePerson(i)
		q := birthday(p)
		total += score(p) + score(q)
	}
	if total != 54777780 {
		panic("struct_records_reuse checksum")
	}
	fmt.Println("assert passed, struct_records_reuse is correct")
}
