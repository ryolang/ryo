# Standard Library

## 1. io Package (Input/Output Operations)

**Purpose:** Provides basic input and output functionalities for interacting with the console, files, and potentially basic networking (if included in the core).

**Essential Functions:**

- `io.print(value: str)`: Prints a string value to the standard output (console). Initially discussed as `io.print` - let's stick with this to namespace I/O functions.
- `io.println(value: str)`: Prints a string value to standard output followed by a newline.
- `io.eprint(value: str)`: Prints a string value to standard error.
- `io.eprintln(value: str)`: Prints a string value to standard error followed by a newline.
- `io.readln() -> IoError!str`: Reads a line of text from standard input. Returns the line on success or an `IoError` on failure (e.g., EOF).
- `io.open_file(path: str, mode: str) -> IoError!File`: Opens a file at the given path in the specified mode (e.g., `"r"` for read, `"w"` for write). Returns a `File` object on success or an `IoError` on failure.
- `File` struct (within io):
  - `File.read_all() -> IoError!str`: Reads the entire content of a file as a string.
  - `File.read_line() -> IoError!str`: Reads a single line from a file.
  - `File.write_all(content: str) -> IoError!()`: Writes the entire content string to the file.
  - `File.write_line(line: str) -> IoError!()`: Writes a single line to the file.
  - `File.close() -> IoError!()`: Closes the file.

## 2. str Package (String Manipulation)

**Purpose:** Provides common string manipulation functions.

**Essential Functions:**

- `str.len(s: str) -> int`: Returns the length of a string (number of characters).
- `str.concat(s1: str, s2: str) -> str`: Concatenates two strings. (Though `+` operator for strings might also be considered for basic concatenation).
- `str.contains(text: str, substring: str) -> bool`: Checks if a string contains a substring.
- `str.starts_with(text: str, prefix: str) -> bool`: Checks if a string starts with a prefix.
- `str.ends_with(text: str, suffix: str) -> bool`: Checks if a string ends with a suffix.
- `str.to_int(s: str) -> ParseError!int`: Tries to parse a string into an integer. Returns the integer on success or a `ParseError` on failure.
- `str.to_float(s: str) -> ParseError!float`: Tries to parse a string into a float. Returns the float on success or a `ParseError` on failure.
- `str.format(format_string: str, ...args) -> str`: String formatting function (like f-strings or sprintf).

## 3. conv Package (Type Conversions)

**Purpose:** Provides explicit type conversion functions, especially for basic types.

**Essential Functions:**

- `conv.to_str(value: any) -> str`: Converts a value of various basic types (`int`, `float`, `bool`, etc.) to its string representation. Handles basic types; for structs/enums, users would likely define `to_string()` methods.
- `conv.to_int(value: float) -> ConversionError!int`: Converts a float to an integer. Returns an error for `NaN`, `Infinity`, or out-of-range values.
- `conv.to_float(value: int) -> float`: Converts an integer to a float.
- `conv.to_bool(value: any) -> ConversionError!bool`: Attempts to convert various types to boolean (e.g., `"true"`, `"false"`, `1`, `0`, empty string). Returns an error for invalid input.

## 4. list Package (List Utilities)

**Purpose:** Provides utility functions that operate on list data structures. (Note: Basic list operations like indexing, appending, etc., might be built-in operators/methods - this package is for utility functions).

**Essential Functions:**

- `list.len(lst: list[T]) -> int`: Returns the length of a list.
- `list.is_empty(lst: list[T]) -> bool`: Checks if a list is empty.
- `list.first(lst: list[T]) -> ?T`: Returns the first element of a list, or `none` if the list is empty.
- `list.last(lst: list[T]) -> ?T`: Returns the last element of a list, or `none` if the list is empty.
- `list.append[T](lst: list[T], item: T) -> list[T]`: Appends an item to the end of a list and returns the modified list (or modifies in-place if lists are mutable, clarify mutability rules for lists).
- `list.map[T, U](lst: list[T], f: fn(T) -> U) -> list[U]`: Applies a function `f` to each element of the list and returns a new list with the results (functional map).
- `list.filter[T](lst: list[T], predicate: fn(T) -> bool) -> list[T]`: Filters a list, returning a new list containing only elements that satisfy the predicate function (functional filter).

## 5. map Package (Map Utilities)

**Purpose:** Provides utility functions that operate on map data structures. (Similar to list package - basic map operations would be built-in).

**Essential Functions:**

- `map.len(mp: map[K, V]) -> int`: Returns the number of key-value pairs in a map.
- `map.is_empty(mp: map[K, V]) -> bool`: Checks if a map is empty.
- `map.keys[K, V](mp: map[K, V]) -> list[K]`: Returns a list of keys from the map.
- `map.values[K, V](mp: map[K, V]) -> list[V]`: Returns a list of values from the map.
- `map.get[K, V](mp: map[K, V], key: K) -> ?V`: Retrieves the value associated with a key. Returns the value if key exists, `none` otherwise.
- `map.contains_key[K, V](mp: map[K, V], key: K) -> bool`: Checks if a map contains a specific key.

## 6. time Package (Time and Duration)

**Purpose:** Provides basic time-related functionalities.

**Essential Functions:**

- `time.now_ms() -> int`: Returns the current time in milliseconds since some epoch (e.g., Unix epoch). Useful for measuring durations and timestamps.
- `time.duration_ms(milliseconds: int) -> Duration`: Creates a `Duration` object representing a duration in milliseconds (for clarity and type safety when dealing with time durations - `Duration` could be a simple struct).

## References

- https://buzz-lang.dev/reference/std/std.html
- https://ziglang.org/documentation/master/std/
- https://pkg.go.dev/std
- https://doc.rust-lang.org/std/
- https://docs.python.org/3/library/index.html
- https://docs.modular.com/mojo/lib/