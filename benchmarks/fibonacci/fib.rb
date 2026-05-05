def fibonacci(n)
  return n if n <= 1
  fibonacci(n - 1) + fibonacci(n - 2)
end

result = fibonacci(40)
puts "fib(40) = #{result}"
