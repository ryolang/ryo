package main

import "fmt"

type Person struct {
	Name string
	Age  int
}

func makePerson(i int) Person {
	return Person{Name: fmt.Sprintf("user%d", i), Age: 20 + i%50}
}

func birthday(p *Person) {
	p.Age++
}

func score(p Person) int {
	return len(p.Name) + p.Age
}

func main() {
	total := 0
	for i := range 500000 {
		p := makePerson(i)
		birthday(&p)
		total += score(p)
	}
	if total != 27638890 {
		panic("struct_records_inout checksum")
	}
	fmt.Println("assert passed, struct_records_inout is correct")
}
