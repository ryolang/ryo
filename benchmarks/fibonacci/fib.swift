func fibonacci(n: Int) -> Int {
    if n <= 1 {
        return n
    }
    return fibonacci(n: n - 1) + fibonacci(n: n - 2)
}

let result = fibonacci(n: 40)
print("fib(40) = \(result)")
