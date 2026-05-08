fun fibonacci(n: Int): Int {
    if (n <= 1) {
        return n
    }
    return fibonacci(n - 1) + fibonacci(n - 2)
}

fun main() {
    val result = fibonacci(40)
    println("fib(40) = $result")
}
