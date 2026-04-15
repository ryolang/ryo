--- START OF FILE concurrency_proposal.md ---

# Ryo Concurrency Model Proposal: Task, Future, and Channels

## 1. Rationale: Why Replace `async/await` Function Coloring

The Task/Future/Channel model, inspired by Go and adapted for Ryo's ownership system, provides a superior developer experience by eliminating function coloring while maintaining safety and performance. This aligns perfectly with Ryo's core goal of **Python-like Simplicity and Developer Experience (DX)**.

## 2. Core Primitives and Safety

Ryo's concurrency is built on three orthogonal primitives: **Task**, **Future**, and **Channel**.

### 2.1 Task and future (Execution and Return)

Tasks are Ryo's lightweight, non-OS-thread concurrency unit (like Go's goroutines).

| Primitive | Ryo Syntax | Type Signature | Semantics |
| :--- | :--- | :--- | :--- |
| **Run** | `task.run(fn): ...` | `fn(f: fn() -> T) -> future[T]` | Executes the function `f` concurrently. Returns a **`future[T]`** to retrieve the result. |
| **Spawn Detached** | `task.spawn_detached: ...` | `fn(f: fn() -> void) -> void` | **Fire-and-forget (explicit opt-out)**. No future returned. Errors logged to stderr. Cancelled on process exit. |
| **Await** | `fut.await` | **`future[T]`** | A handle to a potentially pending value of type `T`. The `.await` method **suspends the current task** until the value is ready (it **does not block the OS thread**). |

**Ownership Safety:** Task closures implicitly capture by **move** — the compiler enforces this because tasks may outlive the spawning scope. To share data across tasks, use `shared[T]` with `.clone()`. Dropping a `future[T]` without `.await` cancels the associated task.

### 2.2 Channels (Communication and Synchronization)

Channels are the idiomatic, memory-safe way to communicate and synchronize between tasks by **transferring ownership** of data.

| Primitive | Ryo Syntax | Semantics |
| :--- | :--- | :--- |
| **Create** | `tx, rx = std.channel.create[T]()` | Creates a pair of `sender[T]` and `receiver[T]` for type `T`. |
| **Send** | `tx.send(value)` | Sends `value` to the channel. `value` is **moved** into the channel. The sending task may **suspend** if the channel's buffer is full. |
| **Receive** | `rx.recv()` | **Suspends the current task** until a message is available. Returns the received value, gaining ownership of it. |

### 2.3 Error Integration (Corrected Syntax)

The `future` type integrates seamlessly with Ryo's error system, using the correct lowercase and bracket syntax:

*   **Type:** A future that can fail is represented as **`future[!T]`** (using the error-union prefix `!`).
*   **Unwrap:** The `.await` operation is designed to work with the `try` operator:
    ```ryo
    fn fetch() -> future[!str]: ...
    body: str = try fetch().await # .await unwraps the outer future, try unwraps the inner Error Union.
    ```

## 3. Concurrency Control Flow and Utilities

### 3.1 Non-Deterministic Waiting (`select`)

`select` is a structural keyword for waiting on multiple, mixed concurrency primitives.

```ryo
select:
    case res = fut.await:          # Wait for a future
        # ...
    case msg = rx.recv():          # Wait for a channel message
        # ...
    case task.delay(10s).await:        # Wait for a timeout
        # ...
```

### 3.2 Task Grouping and Management

| Primitive | Ryo Syntax | Type Signature | Semantics |
| :--- | :--- | :--- | :--- |
| **Gather** | `task.gather([f1, f2])` | `fn(list[future[!T]]) -> future[Tuple]` | Waits for a list of **heterogeneous** futures. |
| **Join** | `task.join([list_of_futures])` | `fn(list[future[T]]) -> future[list[T]]` | Waits for a list of **homogeneous** futures. |
| **Any** | `task.any([f1, f2])` | `fn(list[future[T]]) -> future[T]` | Waits for the **first** future to complete. |
| **Delay** | `task.delay(duration)` | `fn(duration) -> future[void]` | **Suspends the current task** for the specified duration. |
| **Timeout** | `task.timeout(duration, fut)` | `fn(duration, future[!T]) -> future[!T]` | Fails with a `Timeout` error if the future does not complete in time. |
| **Cancel** | `fut.cancel()` | `fn(future[T]) -> void` | Attempts to stop the associated task. |
| **Group** | `task.group().spawn(fn)` | `struct task_group` | Manages the lifetime of child tasks (RAII-based scoping). |

## 4. Specification Updates

### Section 4.7: Built-in Collections (Addition)

*   **Add** the `future[T]` type to the list of built-in fundamental types.

### Section 9: Concurrency Model (Full Replacement)

*   **Replace** the existing `async/await` section with this **Task/Future/Channel** model.

### Section 13: Standard Library (Updates)

*   `std.task`: Contains all `task.` and `future.` primitives.
*   `std.channel`: Contains `sender[T]`, `receiver[T]`, `create[T]()`.

--- END OF FILE concurrency_proposal.md ---

This set of examples demonstrates the Ryo concurrency model using the **Task/future/Channel** primitives, adhering to the established conventions (lowercase built-in types, no function coloring).

---

## Ryo Concurrency Examples

### Example 1: Basic Task Execution (`task.run` and `task.spawn_detached`)

This example shows the difference between a detached task (`task.spawn_detached`) and a task that returns a future (`task.run`). Dropping a future cancels the associated task.

```ryo
import std.io
import std.task

# A normal, synchronous function (no 'async' keyword needed)
fn calculate_sum(a: int, b: int) -> int:
    # Simulate a long computation
    task.delay(100ms).await 
    return a + b

fn main():
    io.println("1. Starting main thread.")

    # 1. Fire-and-forget task (explicit opt-out, no future returned)
    task.spawn_detached:
        io.println("   [Detached] Background task started.")
        task.delay(50ms).await
        io.println("   [Detached] Background task finished.")

    # 2. Task that returns a future
    sum_future = task.run:
        io.println("   [Run] Calculation task started.")
        return calculate_sum(10, 20)

    io.println("2. Waiting for calculation to finish...")
    
    # 3. Suspend the current task until the future is ready
    result = sum_future.await
    
    io.println(f"3. Result received: {result}")
    io.println("4. Main thread finished.")

# Expected Output (Tasks run concurrently):
# 1. Starting main thread.
#    [Detached] Background task started.
#    [Run] Calculation task started.
# 2. Waiting for calculation to finish...
#    [Detached] Background task finished.
# 3. Result received: 30
# 4. Main thread finished.
```

### Example 2: Safe Communication with Channels

This example demonstrates how to safely transfer ownership of a string between the main task and a background task using channels.

```ryo
import std.channel
import std.task
import std.io

fn background_worker(tx: sender[str]):
    message = "Hello from the worker task!" # message is owned here
    
    # Ownership of 'message' is MOVED into the channel
    tx.send(message) # The worker task's 'message' variable is now invalid

fn main():
    # Create a buffered channel (e.g., buffer size 1)
    tx, rx = std.channel.create[str](1)

    # Run the worker — future keeps the task alive until recv completes
    worker = task.run:
        background_worker(tx) # 'tx' is implicitly moved into the task closure

    io.println("Waiting for message on channel...")

    # Suspend the main task until a message is received
    # The main task gains ownership of the string
    received_message = try rx.recv() 

    io.println(f"Received: {received_message}")

# Output:
# Waiting for message on channel...
# Received: Hello from the worker task!
```

### Example 3: Non-Deterministic Waiting with `select`

This example uses `select` to wait for the first of three possible events: a result, a message, or a timeout.

```ryo
import std.channel
import std.task
import std.io
import std.time # Assumed module for time constants

fn slow_future() -> future[int]:
    return task.run:
        task.delay(100ms).await # 100ms delay
        return 42

fn main():
    # 1. Setup a future (will resolve in 100ms)
    result_future = slow_future() 
    
    # 2. Setup a channel (will receive a message after 200ms)
    tx, rx = std.channel.create[str]()
    sender = task.run:
        task.delay(200ms).await
        tx.send("Channel message arrived")

    # The 'select' block waits for the FIRST event
    select:
        case res = result_future.await:
            io.println(f"Case 1: Future finished first with result: {res}")
        
        case msg = rx.recv():
            io.println(f"Case 2: Channel message: {msg}")

        case task.delay(50ms).await: # 3. A 50ms timeout
            io.println("Case 3: Timeout occurred first!")

# Expected Output (Case 3 is fastest):
# Case 3: Timeout occurred first!
```

### Example 4: Waiting for All Tasks (`task.join` with Errors)

This example shows how to launch a list of tasks and wait for all of them to complete, correctly handling errors using the `future[!T]` syntax.

```ryo
import std.task
import std.io

# Assumed error type for simplicity
error HttpError(status: int)

fn fetch_status(url: str) -> future[!int]:
    return task.run:
        io.println(f"   [Task] Fetching {url}...")
        task.delay(50ms).await
        
        if url == "fail.com":
            return HttpError(status: 500) # Propagate an error
        
        return 200 # Success status

fn main():
    urls = ["ok1.com", "fail.com", "ok2.com"]
    
    # 1. Create a list of homogeneous futures: list[future[!int]]
    futures_list = [fetch_status(u) for u in urls]

    io.println("Waiting for all tasks to join...")

    # 2. task.join returns a future<!list[int]>
    # The .await unwraps the future, the try unwraps the error union.
    results = task.join(futures_list)

    statuses: list[int] = results.await catch |e|:
        io.println(f"ERROR: A task failed during join: {e.message()}")
        return 1 # Exit with error code

    io.println(f"SUCCESS: All statuses received: {statuses}")
    return 0

# Expected Output:
# Waiting for all tasks to join...
#    [Task] Fetching ok1.com...
#    [Task] Fetching fail.com...
#    [Task] Fetching ok2.com...
# ERROR: A task failed during join: HttpError(status=500)
```
