> **SUPERSEDED (2026-04-15):** The `select/case/default` syntax was chosen over `wait/on/orelse`. See design spec for rationale.

---

That is a very insightful point. You are arguing that since Ryo's `select` is purely for **concurrency management** (Futures, Channels, Timeouts) and **not** for general data destructuring (which is handled by `match`), we should use more context-specific keywords to maximize clarity and avoid confusion with Go's broader `select` use cases.

This aligns perfectly with Ryo's goal of **Making Meaning Transparent**.

---

## Ryo Design: Context-Oriented Concurrency Keywords

We need keywords that clearly indicate **"wait for event"** and **"default action"** within the `select` block.

### 1. Replacing `select` and `case`

| Go/CSP Keyword | Ryo Context | Proposed Ryo Keyword | Rationale |
| :--- | :--- | :--- | :--- |
| **`select`** | Start of a concurrent wait block. | **`wait`** | **Clearer.** Explicitly states the action: "Wait for one of the following events." |
| **`case`** | Marks a concurrent event/branch. | **`on`** | **More Intuitive.** "Wait, and **on** this event, do this." It's more declarative. |
| **`default`** | Go's non-blocking fallback. | **`orelse`** | **Consistent.** Ryo already uses `orelse` for optional/default values. Reusing it for the default wait action is highly consistent. |

### 2. The Final Ryo Concurrency Control Flow

The new syntax would be:

```ryo
# 1. Start the concurrent wait block
wait:
    # 2. Define the concurrent event branches
    on let res = future.await:
        io.println(f"Future finished: {res}")
        return res # Exits the 'wait' block
        
    on let msg = rx.recv():
        io.println(f"Channel message: {msg}")
        # Continues loop if not returned
        
    # 3. Define the non-blocking/timeout fallback
    orelse:
        io.println("No event was immediately ready (non-blocking check)")
        # This block executes if no 'on' event is ready.
```

### 3. Syntax Comparison (Clarity Win)

| Model | Go/Old Ryo Syntax | New Ryo Syntax | Clarity Win |
| :--- | :--- | :--- | :--- |
| **Start Block** | `select:` | **`wait:`** | Explicitly states the intent: **Wait** for an event. |
| **Branch** | `case expr:` | **`on expr:`** | More declarative: **On** this event, execute. |
| **Default** | `default:` | **`orelse:`** | **Consistency Win:** Reuses the `orelse` keyword from Optionals. |

This final set of keywords is highly context-aware, simple, and perfectly aligned with Ryo's DX-first philosophy.

**Action:** Update the specification to use **`wait`**, **`on`**, and **`orelse`** for the concurrency control flow.
