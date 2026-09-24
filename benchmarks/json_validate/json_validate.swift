// json_validate — recursive-descent JSON validator over [UInt8].
// Byte-identical algorithm to json_validate.ryo / .rs / .py / .go:
// every parse function returns the next position after the consumed
// token, or -1 on error.

func skipWs(_ b: [UInt8], _ p: Int) -> Int {
    var i = p
    while i < b.count {
        let c = b[i]
        if c == 32 || c == 9 || c == 10 || c == 13 {
            i += 1
        } else {
            return i
        }
    }
    return i
}

func isHexDigit(_ c: UInt8) -> Bool {
    if c >= 48 && c <= 57 { return true }
    if c >= 65 && c <= 70 { return true }
    if c >= 97 && c <= 102 { return true }
    return false
}

func parseString(_ b: [UInt8], _ p: Int) -> Int {
    if p >= b.count || b[p] != 34 { return -1 }
    var i = p + 1
    while i < b.count {
        let c = b[i]
        if c == 34 {
            return i + 1
        } else if c == 92 {
            if i + 1 >= b.count { return -1 }
            let e = b[i + 1]
            if e == 34 || e == 92 || e == 47 || e == 98 || e == 102 || e == 110 || e == 114 || e == 116 {
                i += 2
            } else if e == 117 {
                if i + 6 > b.count { return -1 }
                var ok = true
                for k in 1..<5 {
                    if !isHexDigit(b[i + 1 + k]) { ok = false }
                }
                if !ok { return -1 }
                i += 6
            } else {
                return -1
            }
        } else if c < 32 {
            return -1
        } else {
            i += 1
        }
    }
    return -1
}

func parseNumber(_ b: [UInt8], _ p: Int) -> Int {
    var i = p
    let n = b.count
    if i < n && b[i] == 45 { i += 1 }
    if i >= n { return -1 }
    if b[i] == 48 {
        i += 1
    } else if b[i] >= 49 && b[i] <= 57 {
        while i < n && b[i] >= 48 && b[i] <= 57 { i += 1 }
    } else {
        return -1
    }
    if i < n && b[i] == 46 {
        i += 1
        if i >= n || !(b[i] >= 48 && b[i] <= 57) { return -1 }
        while i < n && b[i] >= 48 && b[i] <= 57 { i += 1 }
    }
    if i < n && (b[i] == 101 || b[i] == 69) {
        i += 1
        if i < n && (b[i] == 43 || b[i] == 45) { i += 1 }
        if i >= n || !(b[i] >= 48 && b[i] <= 57) { return -1 }
        while i < n && b[i] >= 48 && b[i] <= 57 { i += 1 }
    }
    return i
}

let litTrue = Array("true".utf8)
let litFalse = Array("false".utf8)
let litNull = Array("null".utf8)

func matchLit(_ b: [UInt8], _ p: Int, _ lit: [UInt8]) -> Int {
    let n = lit.count
    if p + n > b.count { return -1 }
    for k in 0..<n {
        if b[p + k] != lit[k] { return -1 }
    }
    return p + n
}

func parseValue(_ b: [UInt8], _ p: Int) -> Int {
    let i = skipWs(b, p)
    if i >= b.count { return -1 }
    let c = b[i]
    if c == 123 { return parseObject(b, i) }
    if c == 91 { return parseArray(b, i) }
    if c == 34 { return parseString(b, i) }
    if c == 116 { return matchLit(b, i, litTrue) }
    if c == 102 { return matchLit(b, i, litFalse) }
    if c == 110 { return matchLit(b, i, litNull) }
    if c == 45 || (c >= 48 && c <= 57) { return parseNumber(b, i) }
    return -1
}

func parseArray(_ b: [UInt8], _ p: Int) -> Int {
    var i = skipWs(b, p + 1)
    if i < b.count && b[i] == 93 { return i + 1 }
    i = parseValue(b, i)
    if i < 0 { return -1 }
    while true {
        i = skipWs(b, i)
        if i >= b.count { return -1 }
        let c = b[i]
        if c == 44 {
            i = parseValue(b, i + 1)
            if i < 0 { return -1 }
        } else if c == 93 {
            return i + 1
        } else {
            return -1
        }
    }
}

func parseObject(_ b: [UInt8], _ p: Int) -> Int {
    var i = skipWs(b, p + 1)
    if i < b.count && b[i] == 125 { return i + 1 }
    i = parseString(b, i)
    if i < 0 { return -1 }
    i = skipWs(b, i)
    if i >= b.count || b[i] != 58 { return -1 }
    i = parseValue(b, i + 1)
    if i < 0 { return -1 }
    while true {
        i = skipWs(b, i)
        if i >= b.count { return -1 }
        let c = b[i]
        if c == 44 {
            i = skipWs(b, i + 1)
            i = parseString(b, i)
            if i < 0 { return -1 }
            i = skipWs(b, i)
            if i >= b.count || b[i] != 58 { return -1 }
            i = parseValue(b, i + 1)
            if i < 0 { return -1 }
        } else if c == 125 {
            return i + 1
        } else {
            return -1
        }
    }
}

func validate(_ b: [UInt8]) -> Bool {
    let p = parseValue(b, 0)
    if p < 0 { return false }
    return skipWs(b, p) == b.count
}

func checkValid(_ s: String) {
    precondition(validate(Array(s.utf8)), "expected valid")
}

func checkInvalid(_ s: String) {
    precondition(!validate(Array(s.utf8)), "expected invalid")
}

checkValid("{}")
checkValid("[]")
checkValid(" {\"a\": [1, 2.5e-3, true, null], \"b\": \"x\"} ")
checkValid("{\"esc\": \"a\\\"b\\\\c\\/\\b\\f\\n\\r\\t\"}")
checkValid("\"A\u{e9}\"")
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

var s = "["
for i in 0..<30000 {
    if i > 0 { s += "," }
    s += "{\"id\": "
    s += String(i)
    s += ", \"name\": \"user"
    s += String(i)
    s += "\", \"tags\": [\"a\", \"b\"], \"active\": true, \"score\": 1.5, \"misc\": null}"
}
s += "]"

let raw = Array(s.utf8)

var ok = true
for _ in 0..<12 {
    if !validate(raw) { ok = false }
}
precondition(ok, "json_validate big document")
print("assert passed, json_validate is correct")
