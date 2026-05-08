defmodule Fibonacci do
  def fib(n) when n <= 1 do
    n
  end

  def fib(n) do
    fib(n - 1) + fib(n - 2)
  end
end

result = Fibonacci.fib(40)
IO.puts("fib(40) = #{result}")
