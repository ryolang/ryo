// json_validate — recursive-descent JSON validator over []byte.
// Byte-identical algorithm to json_validate.ryo / .rs / .py / .swift:
// every parse function returns the next position after the consumed
// token, or -1 on error.
package main

import (
	"fmt"
	"strings"
)

func skipWs(b []byte, p int) int {
	i := p
	for i < len(b) {
		c := b[i]
		if c == 32 || c == 9 || c == 10 || c == 13 {
			i++
		} else {
			return i
		}
	}
	return i
}

func isHexDigit(c byte) bool {
	if c >= 48 && c <= 57 {
		return true
	}
	if c >= 65 && c <= 70 {
		return true
	}
	if c >= 97 && c <= 102 {
		return true
	}
	return false
}

func parseString(b []byte, p int) int {
	if p >= len(b) || b[p] != 34 {
		return -1
	}
	i := p + 1
	for i < len(b) {
		c := b[i]
		if c == 34 {
			return i + 1
		} else if c == 92 {
			if i+1 >= len(b) {
				return -1
			}
			e := b[i+1]
			if e == 34 || e == 92 || e == 47 || e == 98 || e == 102 || e == 110 || e == 114 || e == 116 {
				i += 2
			} else if e == 117 {
				if i+6 > len(b) {
					return -1
				}
				ok := true
				for k := range 4 {
					if !isHexDigit(b[i+2+k]) {
						ok = false
					}
				}
				if !ok {
					return -1
				}
				i += 6
			} else {
				return -1
			}
		} else if c < 32 {
			return -1
		} else {
			i++
		}
	}
	return -1
}

func parseNumber(b []byte, p int) int {
	i := p
	n := len(b)
	if i < n && b[i] == 45 {
		i++
	}
	if i >= n {
		return -1
	}
	if b[i] == 48 {
		i++
	} else if b[i] >= 49 && b[i] <= 57 {
		for i < n && b[i] >= 48 && b[i] <= 57 {
			i++
		}
	} else {
		return -1
	}
	if i < n && b[i] == 46 {
		i++
		if i >= n || !(b[i] >= 48 && b[i] <= 57) {
			return -1
		}
		for i < n && b[i] >= 48 && b[i] <= 57 {
			i++
		}
	}
	if i < n && (b[i] == 101 || b[i] == 69) {
		i++
		if i < n && (b[i] == 43 || b[i] == 45) {
			i++
		}
		if i >= n || !(b[i] >= 48 && b[i] <= 57) {
			return -1
		}
		for i < n && b[i] >= 48 && b[i] <= 57 {
			i++
		}
	}
	return i
}

var (
	litTrue  = []byte("true")
	litFalse = []byte("false")
	litNull  = []byte("null")
)

func matchLit(b []byte, p int, lit []byte) int {
	n := len(lit)
	if p+n > len(b) {
		return -1
	}
	for k := range n {
		if b[p+k] != lit[k] {
			return -1
		}
	}
	return p + n
}

func parseValue(b []byte, p int) int {
	i := skipWs(b, p)
	if i >= len(b) {
		return -1
	}
	c := b[i]
	if c == 123 {
		return parseObject(b, i)
	}
	if c == 91 {
		return parseArray(b, i)
	}
	if c == 34 {
		return parseString(b, i)
	}
	if c == 116 {
		return matchLit(b, i, litTrue)
	}
	if c == 102 {
		return matchLit(b, i, litFalse)
	}
	if c == 110 {
		return matchLit(b, i, litNull)
	}
	if c == 45 || (c >= 48 && c <= 57) {
		return parseNumber(b, i)
	}
	return -1
}

func parseArray(b []byte, p int) int {
	i := skipWs(b, p+1)
	if i < len(b) && b[i] == 93 {
		return i + 1
	}
	i = parseValue(b, i)
	if i < 0 {
		return -1
	}
	for {
		i = skipWs(b, i)
		if i >= len(b) {
			return -1
		}
		c := b[i]
		if c == 44 {
			i = parseValue(b, i+1)
			if i < 0 {
				return -1
			}
		} else if c == 93 {
			return i + 1
		} else {
			return -1
		}
	}
}

func parseObject(b []byte, p int) int {
	i := skipWs(b, p+1)
	if i < len(b) && b[i] == 125 {
		return i + 1
	}
	i = parseString(b, i)
	if i < 0 {
		return -1
	}
	i = skipWs(b, i)
	if i >= len(b) || b[i] != 58 {
		return -1
	}
	i = parseValue(b, i+1)
	if i < 0 {
		return -1
	}
	for {
		i = skipWs(b, i)
		if i >= len(b) {
			return -1
		}
		c := b[i]
		if c == 44 {
			i = skipWs(b, i+1)
			i = parseString(b, i)
			if i < 0 {
				return -1
			}
			i = skipWs(b, i)
			if i >= len(b) || b[i] != 58 {
				return -1
			}
			i = parseValue(b, i+1)
			if i < 0 {
				return -1
			}
		} else if c == 125 {
			return i + 1
		} else {
			return -1
		}
	}
}

func validate(b []byte) bool {
	p := parseValue(b, 0)
	if p < 0 {
		return false
	}
	return skipWs(b, p) == len(b)
}

func checkValid(s string) {
	if !validate([]byte(s)) {
		panic("expected valid")
	}
}

func checkInvalid(s string) {
	if validate([]byte(s)) {
		panic("expected invalid")
	}
}

func main() {
	checkValid("{}")
	checkValid("[]")
	checkValid(" {\"a\": [1, 2.5e-3, true, null], \"b\": \"x\"} ")
	checkValid("{\"esc\": \"a\\\"b\\\\c\\/\\b\\f\\n\\r\\t\"}")
	checkValid("\"Aé\"")
	checkValid("[0, -1, 3.14, 1e10, 2.5E-3, -0.5e+2]")
	checkValid("{\"a\": {\"b\": {\"c\": [true, [false, [null]]]}}}")
	checkValid("123")

	checkInvalid("")
	checkInvalid(" ")
	checkInvalid("{\"a\":}")
	checkInvalid("[1,]")
	checkInvalid("{\"a\" 1}")
	checkInvalid("01")
	checkInvalid("\"abc")
	checkInvalid("{\"a\": 1}]")
	checkInvalid("\"\\x\"")
	checkInvalid("[1 2]")
	checkInvalid("tru")
	checkInvalid("nulll")
	checkInvalid("{a: 1}")
	checkInvalid("\"a\tb\"")
	checkInvalid("1.")
	checkInvalid("1e")
	checkInvalid("-")

	var sb strings.Builder
	sb.WriteString("[")
	for i := range 30000 {
		if i > 0 {
			sb.WriteString(",")
		}
		sb.WriteString("{\"id\": ")
		sb.WriteString(fmt.Sprint(i))
		sb.WriteString(", \"name\": \"user")
		sb.WriteString(fmt.Sprint(i))
		sb.WriteString("\", \"tags\": [\"a\", \"b\"], \"active\": true, \"score\": 1.5, \"misc\": null}")
	}
	sb.WriteString("]")

	raw := []byte(sb.String())

	ok := true
	for range 12 {
		if !validate(raw) {
			ok = false
		}
	}
	if !ok {
		panic("json_validate big document")
	}
	fmt.Println("assert passed, json_validate is correct")
}
