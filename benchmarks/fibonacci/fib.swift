func fibonacci(n: UInt32) -> UInt32 {
    if n <= 1 {
        return n
    }
    return fibonacci(n: n - 1) + fibonacci(n: n - 2)
}

let result = fibonacci(n: 40)
print("fib(40) = \(result)")
