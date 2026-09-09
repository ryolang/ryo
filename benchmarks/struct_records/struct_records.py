class Person:
    __slots__ = ("name", "age")

    def __init__(self, name, age):
        self.name = name
        self.age = age


def make_person(i):
    return Person("user" + str(i), 20 + i % 50)


def birthday(p):
    return Person("user" + str(p.age), p.age + 1)


def score(p):
    return len(p.name) + p.age


def main():
    total = 0
    for i in range(500000):
        p = make_person(i)
        q = birthday(p)
        total += score(q)
    assert total == 25750000, "struct_records checksum"
    print("assert passed, struct_records is correct")


main()
