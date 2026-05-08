function fibonacci(n)
    n <= 1 && return n
    fibonacci(n - 1) + fibonacci(n - 2)
end

result = fibonacci(40)
println("fib(40) = $result")
