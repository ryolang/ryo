def fibonacci(n):
    if n <= 1:
        return n
    return fibonacci(n - 1) + fibonacci(n - 2)

if __name__ == "__main__":
    result = fibonacci(40)
    print(f"fib(40) = {result}")
