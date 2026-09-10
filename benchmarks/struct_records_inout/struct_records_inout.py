class Person:
    __slots__ = ("age", "name")

    def __init__(self, name, age):
        self.name = name
        self.age = age


def make_person(i):
    return Person("user" + str(i), 20 + i % 50)


def birthday(p):
    p.age += 1


def score(p):
    return len(p.name) + p.age


def main():
    total = 0
    for i in range(500000):
        p = make_person(i)
        birthday(p)
        total += score(p)
    assert total == 27638890, "struct_records_inout checksum"
    print("assert passed, struct_records_inout is correct")


main()
