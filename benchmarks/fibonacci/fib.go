package main

import "fmt"

func fibonacci(n uint32) uint32 {
	if n <= 1 {
		return n
	}
	return fibonacci(n-1) + fibonacci(n-2)
}

func main() {
	result := fibonacci(40)
	fmt.Printf("fib(40) = %d\n", result)
}
