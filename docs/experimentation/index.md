# Getting Started Guide

Welcome to ⚡ Ryo **/ˈraɪoʊ/** (Rye-oh), a programming language designed for simplicity and readability, drawing inspiration from Python and Rust safety. Ryo aims to be easy to learn and use, letting you focus on your ideas and code, without getting bogged down in unnecessary complexity.

This guide will walk you through the basics of Ryo and help you write your first programs. Let's get started!

## 1. Setting Up Ryo

*(Placeholder - Installation Instructions will go here when Ryo is ready)*

For now, let's assume you have Ryo installed and ready to run. We'll focus on writing Ryo code.

## 2. Your First Ryo Program: Hello, World!

Let's start with the classic "Hello, World!" program, Ryo style:

```ryo
# main.ryo

fn hello(name: str):
    print(f"Hello {name}")

fn main():
    hello("World")
```

**Let's break this down:**

* **`# main.ryo`**: This is a comment indicating the filename is `main.ryo`. Comments in Ryo start with `#` and go to the end of the line, just like in Python.
* **`fn hello(name: str):`**: This line defines a **function** named `hello`.
  * **`fn`**: The keyword `fn` is used to declare a function.
  * **`hello`**: This is the **name** of the function.
  * **`(name: str)`**: This is the **parameter list**.
    * **`name`**: This is the name of the parameter.
    * **`: str`**: This is the **type annotation**, specifying that the `name` parameter is expected to be a **string** (`str`). Ryo is a **statically-typed** language, which means types are checked at compile time to catch errors early.
  * **`:`**: The colon `:` marks the end of the function signature and the beginning of the function body.
* **`print(f"Hello {name}")`**: This line is the **body** of the `hello` function. It's **indented** using four spaces. Indentation is important in Ryo to define code blocks, just like in Python!
  * **`print(...)`**: This is a built-in Ryo function to display output to the console.
  * **`f"Hello {name}"`**: This is an **f-string** (formatted string literal), similar to Python. It allows you to embed variables directly into strings by placing them inside curly braces `{}`. In this case, it creates a string "Hello " followed by the value of the `name` parameter.
* **`fn main():`**: This defines the **`main` function**. In Ryo, the `main` function is the **entry point** of your program – where the execution begins.
* **`hello("World")`**: This line inside `main` **calls** the `hello` function, passing the string `"World"` as the argument for the `name` parameter.

**To run this program (once Ryo is set up):**

You would typically use a Ryo compiler or interpreter to execute the `main.ryo` file.

```shell
ryo run main.ryo
```

The output in your console would be:

```
Hello World
```

Congratulations! You've run your first Ryo program.

### A More Practical Example: Temperature Converter

Let's try a slightly more practical example - a temperature converter that demonstrates more of Ryo's features:

```ryo
# temp_converter.ryo

# Define an enumeration for temperature scales
enum TempScale:
    Celsius
    Fahrenheit
    Kelvin

# Define a struct to hold temperature data
struct Temperature:
    value: float
    scale: TempScale

# Define functions to convert between scales
fn to_celsius(temp: Temperature) -> Temperature:
    match temp.scale:
        TempScale.Celsius:
            return temp  # Already Celsius
        TempScale.Fahrenheit:
            return Temperature{
                value: (temp.value - 32.0) * 5.0 / 9.0,
                scale: TempScale.Celsius
            }
        TempScale.Kelvin:
            return Temperature{
                value: temp.value - 273.15,
                scale: TempScale.Celsius
            }

fn to_fahrenheit(temp: Temperature) -> Temperature:
    # First convert to Celsius if needed
    celsius = to_celsius(temp)
    # Then convert Celsius to Fahrenheit
    return Temperature{
        value: celsius.value * 9.0 / 5.0 + 32.0,
        scale: TempScale.Fahrenheit
    }

fn to_kelvin(temp: Temperature) -> Temperature:
    # First convert to Celsius if needed
    celsius = to_celsius(temp)
    # Then convert Celsius to Kelvin
    return Temperature{
        value: celsius.value + 273.15,
        scale: TempScale.Kelvin
    }

fn main():
    # Get user input
    print("Enter a temperature value:")
    input_value = float(input())
    
    print("Enter the scale (C for Celsius, F for Fahrenheit, K for Kelvin):")
    input_scale = input()
    
    # Create Temperature struct based on input
    temp = match input_scale:
        "C" | "c":
            Temperature{value: input_value, scale: TempScale.Celsius}
        "F" | "f":
            Temperature{value: input_value, scale: TempScale.Fahrenheit}
        "K" | "k":
            Temperature{value: input_value, scale: TempScale.Kelvin}
        _:
            print("Invalid scale! Defaulting to Celsius.")
            Temperature{value: input_value, scale: TempScale.Celsius}
    
    # Convert to all scales and display
    celsius = to_celsius(temp)
    fahrenheit = to_fahrenheit(temp)
    kelvin = to_kelvin(temp)
    
    print(f"Celsius: {celsius.value:.2f}°C")
    print(f"Fahrenheit: {fahrenheit.value:.2f}°F")
    print(f"Kelvin: {kelvin.value:.2f}K")
```

This more advanced example showcases:

1. **Enums**: Defining a set of named constants with `enum`
2. **Structs**: Creating a custom data type with `struct`
3. **Pattern Matching**: Using `match` expressions to handle different cases
4. **User Input**: Reading and parsing user input with `input()` and type conversion
5. **String Formatting**: Using format specifiers like `:.2f` in f-strings to control output format
6. **Functions with Return Values**: Defining functions that return values of specific types
7. **Match with Or-Patterns**: Using `|` to match multiple patterns in a single case

To run this program:

```bash
ryo run temp_converter.ryo
```

## 3. Basic Syntax Elements

Let's explore some fundamental building blocks of Ryo syntax.

### Comments

We've already seen comments. Use `#` to start a single-line comment:

```ryo
# This is a comment in Ryo
fn some_function():
    # Another comment inside the function
    print("This code will run") # And a comment at the end of a line
```

### Functions

We define functions using the `fn` keyword, followed by the function name, parameters in parentheses (with type annotations), a colon, and then an indented function body.

```ryo
fn add(x: int, y: int) -> int: # Function to add two integers, returns an integer
    return x + y
```

* **`-> int`**: This after the parameter list is the **return type annotation**. It specifies the type of value the function will return. If a function doesn't explicitly return a value (like `hello` function above), you can omit the `->` and return type annotation.

### Variables

Ryo uses **implicit variable declaration** within functions. You simply use a variable name, and the compiler infers its type based on the value assigned to it.

```ryo
fn example_variables():
    message = "Welcome to Ryo!" # 'message' is implicitly declared as a string
    count = 10                  # 'count' is implicitly declared as an integer
    is_ready = true            # 'is_ready' is implicitly declared as a boolean

    print(message)
    print(count)
    print(is_ready)
```

### Data Types

Ryo supports common basic data types:

* **`int`**: Integer numbers (e.g., `10`, `-5`, `0`).
* **`float`**: Floating-point numbers (e.g., `3.14`, `-2.5`, `0.0`).
* **`bool`**: Boolean values: `true` or `false`.
* **`str`**: Strings of text (e.g., `"Hello"`, `"Ryo is fun!"`).

### Operators

Ryo provides standard operators:

* **Arithmetic Operators**: `+` (addition), `-` (subtraction), `*` (multiplication), `/` (division), `%` (modulo).
* **Comparison Operators**: `==` (equal to), `!=` (not equal to), `<` (less than), `>` (greater than), `<=` (less than or equal to), `>=` (greater than or equal to).
* **Logical Operators**: `and`, `or`, `not`.

### Control Flow - `if`, and `else`

Ryo uses `if` and `else` statements for conditional execution. Indentation defines the code blocks.

```ryo
fn check_number(number: int):
    if number > 0:
        print("Number is positive")
    elif number == 0: # 'elif' for "else if"
        print("Number is zero")
    else:
        print("Number is negative")
```

* **`elif`**: Ryo uses `elif` for "else if" conditions, similar to Python.
* **`else`**: The `else` block executes if none of the `if` or `elif` conditions are true.

### Control Flow - `for` loop

Ryo provides a `for` loop for iteration:

```ryo
fn count_to(limit: int):
    for i in range(0, limit): # range(start, end) generates numbers from start up to (but not including) end
        print(i)
```

* **`range(start, end)`**: This built-in function generates a sequence of numbers starting from `start` and going up to (but not including) `end`.

## 4. Working with Imports

In Ryo, you can import code from other files and packages using the `import` statement. This helps organize your code into reusable modules.

### Basic Import Syntax

```ryo
# Import a local module
import my_module

# Use a function from the imported module
result = my_module.some_function()
```

### Importing Multiple Modules from a Package

You can group related modules using curly braces:

```ryo
# Import math and strings modules from the utils package
import utils.{math, strings}

# Use functions from these modules
sum_result = math.sum(2, 3)
capitalized = strings.capitalize("hello")
```

### Importing External Packages

For external packages installed via the package manager, use the `pkg:` prefix:

```ryo
# Import an external package
import pkg:http

# Use the package
response = http.get("https://api.example.com/data")
```

### Project Structure Example

A typical Ryo project might be structured like this:

```
my_app/
├── ryo.toml           # Project configuration
└── src/
    ├── main.ryo       # Entry point
    ├── utils/
    │   ├── math.ryo   # package utils
    │   └── strings.ryo# package utils
    └── api/
        └── v1/
            └── users.ryo # package api.v1.users
```

### Complete Import Example

```ryo
# main.ryo
import utils.{math, strings}
import api.v1.users
import pkg:http

fn main():
    print(math.sum(2, 3))                 # 5
    name = strings.capitalize("john")     # "John"
    users = users.get_all()               # Function from api/v1/users.ryo
    response = http.get("https://api.example.com/data")
    print(response.body)
```

For more detailed information about imports and module organization, see the [Deep Dive Guide](deep_guide.md#imports).

## 5. Simple Error Handling

Ryo provides a basic way to handle errors with the `Result[T, E]` type:

```ryo
fn divide(numerator: float, denominator: float) -> Result[float, Err]:
    if denominator == 0.0:
        return Err("Cannot divide by zero") # Return an error
    else:
        return Ok(numerator / denominator) # Return a successful result
```

You can handle the result using a `match` expression:

```ryo
fn main():
    result = divide(10.0, 2.0)
    match result:
        Ok(value):
            print(f"Division successful: {value}")
        Err(error_message):
            print(f"Division failed: {error_message}")
```

## 6. Command-Line Tools

Here are the basic commands for using Ryo:

### Running a Ryo Program

```bash
ryo run <main_file.ryo> [arguments...]
```

### Using the Ryo REPL (Interactive Mode)

```bash
ryo console
```

or simply:

```bash
ryo
```

Example REPL session:

```
ryo
>> fn add(x: int, y: int) -> int:
	  return x + y
>> add(5, 3)
8
>> print("Hello from REPL!")
Hello from REPL!
>> exit
```

## 7. Package Management

Ryo comes with a built-in package manager that makes it easy to use third-party libraries in your projects.

### Installing Packages

To install a package, use the `ryo pkg add` command:

```bash
ryo pkg add http
```

This command installs the `http` package and adds it to your project's dependencies.

### Using Packages in Your Code

Once a package is installed, you can import and use it in your Ryo code:

```ryo
import pkg:http # Paquete externo instalado (no local)

fn main():
    response = http.get("https://api.example.com/data")
    if response.status == 200:
        print(response.body)
    else:
        print(f"Error: {response.status}")
```

### Creating Your Own Packages

You can also create and publish your own Ryo packages:

```bash
# Create a new package project
ryo pkg new my_package

# Build your package
ryo pkg build

# Publish your package (when ready)
ryo pkg publish
```

For more information about Ryo's package manager, including version constraints, lock files, and working with private repositories, see the [Package Manager Guide](../pkg_manager.md).

## 8. Common Error Messages

When you're getting started, you might encounter these common error messages:

```
Type mismatch: expected 'int', got 'str'
```
This means you're trying to use a string where an integer is required, or vice versa.

```
Cannot find symbol 'variable_name'
```
This means you're trying to use a variable that hasn't been declared yet.

```
Indentation error
```
Ryo uses indentation to define code blocks, so make sure your code is properly indented.

For a comprehensive list of error messages and how to fix them, see our [Troubleshooting Guide](deep_guide.md#troubleshooting).

## 9. Memory Management Overview (TO BE REVIEWED)

Ryo features a modern memory management system that combines the safety of garbage collection with the predictability of manual memory management.

### Ownership Model

Ryo uses an ownership model inspired by Rust but simplified for better learnability:

```ryo
fn create_and_use():
    # 'message' owns this string data
    message = "Hello, Ryo!"
    
    # The 'print' function borrows 'message' temporarily
    print(message)
    
    # 'message' is still valid here
    print(f"Length: {len(message)}")
    
    # When 'message' goes out of scope, its memory is automatically freed
```

### Safe References

Ryo allows you to create references to data without taking ownership:

```ryo
fn update_counter(counter: &mut int):
    # We can modify the value that 'counter' refers to
    counter += 1

fn main():
    count = 0
    update_counter(&mut count)
    print(count)  # Output: 1
```

In this example, `&mut int` denotes a mutable reference to an integer. The function can modify the original value, but doesn't own it.

For more detailed information about Ryo's memory management model, including lifetimes, borrowing rules, and optimization techniques, see the [Memory Management Guide](memory.md).

## 10. Language Comparison

Understanding how Ryo compares to other languages helps clarify its design philosophy and advantages. Here's how Ryo relates to Python and Rust across several key dimensions:


### Philosophy and Design

Ryo combines Python's readability with Rust's safety principles:

| Feature               | Ryo                                 | Python                   | Rust                               | Ryo's Advantage                                         |
| --------------------- | ----------------------------------- | ------------------------ | ---------------------------------- | ------------------------------------------------------- |
| **Code Structure**    | Indentation-based blocks            | Indentation-based blocks | Curly braces                       | More readable code with less visual noise               |
| **Type System**       | Static typing with inference        | Dynamic typing           | Static typing with inference       | Catch errors at compile time while writing concise code |
| **Memory Management** | Ownership with simplified borrowing | Garbage collection       | Ownership with full borrowing      | Safety without complexity; predictable performance      |
| **Error Handling**    | Result types with pattern matching  | Exception-based          | Result types with pattern matching | Explicit error handling without surprises               |

### Code Example: Error Handling

See how Ryo compares when handling potential errors:

**Ryo:**
```ryo
fn divide(a: int, b: int) -> Result[int, Err]:
    if b == 0:
        return Err("Division by zero")
    return Ok(a / b)

# Usage
match divide(10, 2):
    Ok(result):
        print(f"Result: {result}")
    Err(msg):
        print(f"Error: {msg}")
```

**Python:**
```python
def divide(a, b):
    try:
        return a / b
    except ZeroDivisionError:
        return "Division by zero"
        
# Usage
result = divide(10, 2)
if isinstance(result, str):
    print(f"Error: {result}")
else:
    print(f"Result: {result}")
```

**Rust:**
```rust
fn divide(a: i32, b: i32) -> Result<i32, String> {
    if b == 0 {
        return Err(String::from("Division by zero"));
    }
    Ok(a / b)
}

// Usage
match divide(10, 2) {
    Ok(result) => println!("Result: {}", result),
    Err(msg) => println!("Error: {}", msg),
}
```

### When to Choose Ryo

* **Choose Ryo over Python when:** You need better performance, compile-time type safety, or more predictable memory usage without sacrificing readability.

* **Choose Ryo over Rust when:** You want memory safety and performance but prefer a gentler learning curve and more Python-like syntax.

* **Ryo is ideal for:** Web services, data processing, CLI tools, game development, and systems that need both safety and simplicity.

For more detailed comparisons and migration tips, see our [Language Comparison Guide](documentation.md).

## 11. Next Steps

Congratulations on completing this Getting Started guide! You've learned the basics of Ryo syntax.

**Where to go from here:**

* **Explore more examples:** Look for more Ryo code examples to see different features in action.
* **Practice writing Ryo code:** Start writing your own simple Ryo programs. Try to solve small problems using the features you've learned.
* **Read the full Ryo documentation:** Dive deeper into Ryo with our comprehensive documentation:
  * [Standard Library Reference](../std.md) - Explore Ryo's built-in functions and modules
  * [Deep Dive Guide](deep_guide.md) - Advanced language features and concepts
  * [Language Specification Summary](spec_summary.md) - Technical details of Ryo's design
  * [Package Manager Guide](../pkg_manager.md) - Learn how to use Ryo's package ecosystem
  * [Memory Management](memory.md) - Understanding how Ryo handles memory
  * [Cheat Sheet](cheats.md) - Quick reference for Ryo syntax and common patterns
* **Join the Ryo community:** Connect with other Ryo developers to ask questions, share your projects, and contribute to the language's development.
  * Join our Discord server: [discord.gg/ryo](https://discord.gg/ryo-lang)
  * Star us on GitHub: [github.com/ryolang/ryo](https://github.com/ryolang/ryo)
  * Follow us on Twitter: [@RyoLang](https://twitter.com/ryo-lang) 