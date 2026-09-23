# json_validate — recursive-descent JSON validator over a bytes object.
# Byte-identical algorithm to json_validate.ryo / json_validate.rs:
# every parse function returns the next position after the consumed
# token, or -1 on error.


def skip_ws(b, p):
    i = p
    while i < len(b):
        c = b[i]
        if c == 32 or c == 9 or c == 10 or c == 13:
            i += 1
        else:
            return i
    return i


def is_hex_digit(c):
    if 48 <= c <= 57:
        return True
    if 65 <= c <= 70:
        return True
    if 97 <= c <= 102:
        return True
    return False


def parse_string(b, p):
    if p >= len(b) or b[p] != 34:
        return -1
    i = p + 1
    while i < len(b):
        c = b[i]
        if c == 34:
            return i + 1
        elif c == 92:
            if i + 1 >= len(b):
                return -1
            e = b[i + 1]
            if e in (34, 92, 47, 98, 102, 110, 114, 116):
                i += 2
            elif e == 117:
                if i + 6 > len(b):
                    return -1
                ok = True
                for k in range(1, 5):
                    if not is_hex_digit(b[i + 1 + k]):
                        ok = False
                if not ok:
                    return -1
                i += 6
            else:
                return -1
        elif c < 32:
            return -1
        else:
            i += 1
    return -1


def parse_number(b, p):
    i = p
    n = len(b)
    if i < n and b[i] == 45:
        i += 1
    if i >= n:
        return -1
    if b[i] == 48:
        i += 1
    elif 49 <= b[i] <= 57:
        while i < n and 48 <= b[i] <= 57:
            i += 1
    else:
        return -1
    if i < n and b[i] == 46:
        i += 1
        if i >= n or not (48 <= b[i] <= 57):
            return -1
        while i < n and 48 <= b[i] <= 57:
            i += 1
    if i < n and (b[i] == 101 or b[i] == 69):
        i += 1
        if i < n and (b[i] == 43 or b[i] == 45):
            i += 1
        if i >= n or not (48 <= b[i] <= 57):
            return -1
        while i < n and 48 <= b[i] <= 57:
            i += 1
    return i


def match_lit(b, p, lit):
    n = len(lit)
    if p + n > len(b):
        return -1
    for k in range(0, n):
        if b[p + k] != lit[k]:
            return -1
    return p + n


def parse_value(b, p):
    i = skip_ws(b, p)
    if i >= len(b):
        return -1
    c = b[i]
    if c == 123:
        return parse_object(b, i)
    if c == 91:
        return parse_array(b, i)
    if c == 34:
        return parse_string(b, i)
    if c == 116:
        return match_lit(b, i, b"true")
    if c == 102:
        return match_lit(b, i, b"false")
    if c == 110:
        return match_lit(b, i, b"null")
    if c == 45 or (48 <= c <= 57):
        return parse_number(b, i)
    return -1


def parse_array(b, p):
    i = skip_ws(b, p + 1)
    if i < len(b) and b[i] == 93:
        return i + 1
    i = parse_value(b, i)
    if i < 0:
        return -1
    while True:
        i = skip_ws(b, i)
        if i >= len(b):
            return -1
        c = b[i]
        if c == 44:
            i = parse_value(b, i + 1)
            if i < 0:
                return -1
        elif c == 93:
            return i + 1
        else:
            return -1


def parse_object(b, p):
    i = skip_ws(b, p + 1)
    if i < len(b) and b[i] == 125:
        return i + 1
    i = parse_string(b, i)
    if i < 0:
        return -1
    i = skip_ws(b, i)
    if i >= len(b) or b[i] != 58:
        return -1
    i = parse_value(b, i + 1)
    if i < 0:
        return -1
    while True:
        i = skip_ws(b, i)
        if i >= len(b):
            return -1
        c = b[i]
        if c == 44:
            i = skip_ws(b, i + 1)
            i = parse_string(b, i)
            if i < 0:
                return -1
            i = skip_ws(b, i)
            if i >= len(b) or b[i] != 58:
                return -1
            i = parse_value(b, i + 1)
            if i < 0:
                return -1
        elif c == 125:
            return i + 1
        else:
            return -1


def validate(b):
    p = parse_value(b, 0)
    if p < 0:
        return False
    return skip_ws(b, p) == len(b)


def check_valid(s):
    assert validate(s.encode()), "expected valid"


def check_invalid(s):
    assert not validate(s.encode()), "expected invalid"


def main():
    check_valid("{}")
    check_valid("[]")
    check_valid(' {"a": [1, 2.5e-3, true, null], "b": "x"} ')
    check_valid('{"esc": "a\\"b\\\\c\\/\\b\\f\\n\\r\\t"}')
    check_valid('"Aé"')
    check_valid("[0, -1, 3.14, 1e10, 2.5E-3, -0.5e+2]")
    check_valid('{"a": {"b": {"c": [true, [false, [null]]]}}}')
    check_valid("123")

    check_invalid("")
    check_invalid(" ")
    check_invalid('{"a":}')
    check_invalid("[1,]")
    check_invalid('{"a" 1}')
    check_invalid("01")
    check_invalid('"abc')
    check_invalid('{"a": 1}]')
    check_invalid('"\\x"')
    check_invalid("[1 2]")
    check_invalid("tru")
    check_invalid("nulll")
    check_invalid("{a: 1}")
    check_invalid('"a\tb"')
    check_invalid("1.")
    check_invalid("1e")
    check_invalid("-")

    parts = ["["]
    for i in range(0, 30000):
        if i > 0:
            parts.append(",")
        parts.append('{"id": ')
        parts.append(str(i))
        parts.append(', "name": "user')
        parts.append(str(i))
        parts.append('", "tags": ["a", "b"], "active": true, "score": 1.5, "misc": null}')
    parts.append("]")
    s = "".join(parts)

    raw = s.encode()

    ok = True
    for _ in range(0, 12):
        if not validate(raw):
            ok = False
    assert ok, "json_validate big document"
    print("assert passed, json_validate is correct")


main()
