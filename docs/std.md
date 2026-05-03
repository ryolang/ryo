# Standard Library

!!! warning "Development Status"
    This STD lib is aspirational only. Aside from the global prelude builtins, the module system is not yet implemented.

## 0. The Prelude (Global Builtins)

**Purpose:** To prevent global namespace pollution, only absolute language fundamentals live here. Everything else requires an explicit module import.

**Essential Functions:**

- `print(msg: str) -> void`: Writes the string directly to standard output. (Note: May eventually move to `io.print` once the module system is stable, but remains global for v0.1 ergonomics).
- `assert(cond: bool, msg: str) -> void`: Asserts a runtime invariant. Desugars to `if not cond: panic(msg)`.
- `panic(msg: str) -> never`: Unrecoverable fatal error. Instantly terminates the process (`exit(101)`) and prints the file, line, and user message to standard error.

## 1. process Package (Execution Environment)

**Purpose:** Interacting with the current running host process and its execution environment. Separated from general OS utilities to provide a clear boundary for process state.

**Essential Functions:**

- `process.exit(code: int) -> never`: Immediately terminates the process with the given exit code. Destructors and cleanup routines are not run.
- `process.args() -> list[str]`: Returns the command-line arguments passed to the program.
- `process.env(key: str) -> ?str`: Retrieves the value of an environment variable, returning `none` if it is not set.

## 2. fs Package (File System)

**Purpose:** Stateless file and path operations. Separated from `io` to isolate OS-level filesystem interactions from stream buffering.

**Essential Functions:**

- `fs.read_file(path: str) -> IoError!str`: Reads an entire file into a string.
- `fs.write_file(path: str, content: str) -> IoError!void`: Overwrites or creates a file with the given string.
- `fs.cwd() -> IoError!str`: Returns the current working directory.

## 3. io Package (Standard Streams)

**Purpose:** For interacting with standard input, error, and general buffering.

**Essential Functions:**

- `io.eprint(msg: str) -> void`: Prints directly to standard error.
- `io.readln() -> IoError!str`: Reads a single line of text from standard input. Returns the line on success or an `IoError` on failure (e.g., EOF).

## 4. str Package (String Manipulation)

**Purpose:** Provides common string manipulation functions.

**Essential Functions:**

- `str.len(s: str) -> int`: Returns the length of a string in bytes (or codepoints).
- `str.contains(s: str, sub: str) -> bool`: Checks if a string contains a substring.
- `str.starts_with(s: str, prefix: str) -> bool`: Checks if a string starts with a prefix.
- `str.ends_with(s: str, suffix: str) -> bool`: Checks if a string ends with a suffix.
- `str.split(s: str, sep: str) -> list[str]`: Splits a string by a given separator.
- `str.trim(s: str) -> str`: Removes leading and trailing whitespace.
- `str.format(format_string: str, ...args) -> str`: String formatting function (like f-strings or sprintf).

## 5. Builtin Type Namespaces (Conversions)

**Purpose:** Replaces the messy generic `conv` package. Conversions live in the namespace of the target type (like Rust/Zig).

**Essential Functions:**

- `int.parse(s: str) -> ParseError!int`: Tries to parse a string into an integer.
- `int.from_float(f: float) -> int`: Truncates a float to an integer.
- `float.parse(s: str) -> ParseError!float`: Tries to parse a string into a float.
- `float.from_int(i: int) -> float`: Converts an integer to a float.
- `bool.parse(s: str) -> ParseError!bool`: Parses a string to boolean (e.g., `"true"`, `"false"`).

## 6. list Package (List Utilities)

**Purpose:** Operations for Ryo's generic list collections.

**Essential Functions:**

- `list.len(lst: list[T]) -> int`: Returns the length of a list.
- `list.push(lst: list[T], item: T) -> void`: Appends an item to the end of a list.
- `list.pop(lst: list[T]) -> ?T`: Removes and returns the last element, or `none` if empty.
- `list.first(lst: list[T]) -> ?T`: Returns the first element of a list, or `none` if empty.
- `list.map[T, U](lst: list[T], f: fn(T) -> U) -> list[U]`: Applies a function `f` to each element.
- `list.filter[T](lst: list[T], predicate: fn(T) -> bool) -> list[T]`: Filters a list.

## 7. map Package (Map Utilities)

**Purpose:** Operations for Ryo's generic map collections.

**Essential Functions:**

- `map.len(mp: map[K, V]) -> int`: Returns the number of key-value pairs in a map.
- `map.get(mp: map[K, V], key: K) -> ?V`: Retrieves the value associated with a key.
- `map.set(mp: map[K, V], key: K, val: V) -> void`: Inserts or updates a key-value pair.
- `map.remove(mp: map[K, V], key: K) -> void`: Removes a key from the map.
- `map.keys(mp: map[K, V]) -> list[K]`: Returns a list of keys from the map.

## 8. time Package (Time and Duration)

**Purpose:** Provides basic time-related functionalities.

**Essential Functions:**

- `time.now_ms() -> int`: Returns the current Unix epoch in milliseconds.
- `time.duration_ms(milliseconds: int) -> Duration`: Creates a `Duration` object representing a duration in milliseconds.

## Missing

- TOML: for the project files

## References

- https://buzz-lang.dev/reference/std/std.html
- https://ziglang.org/documentation/master/std/
- https://pkg.go.dev/std
- https://doc.rust-lang.org/std/
- https://docs.python.org/3/library/index.html
- https://docs.modular.com/mojo/lib/
