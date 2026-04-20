# Analysis Memo: `for condition:` vs Dedicated `while` Keyword

**Date:** 2026-04-20
**Author:** Spec coherence review (Task 11)
**Status:** Pending human review
**Scope:** Should Ryo add a `while` keyword, or keep the current `for condition:` design?

---

## Current Design

Ryo has one loop keyword (`for`) with three forms:

```ryo
for item in items:        # iteration
for i in range(10):       # counted
for counter < 10:         # condition-based (replaces while)
```

The spec rationale (specification.md, line 322):

> One loop keyword (`for`) with three forms keeps the language simple --
> `while` is redundant when `for condition:` exists.

Infinite loops under the current design use `for true:`:

```ryo
for true:
	msg = channel.receive()
	if msg == "quit":
		break
	process(msg)
```

This memo evaluates whether restoring a `while` keyword would benefit Ryo.

---

## 3 Pros of Adding `while`

### Pro 1: Intent Signaling -- Condition-Based vs Iteration

`while` immediately tells the reader "this loop is driven by a condition that may change."
`for` says "this loop iterates over something." Overloading `for` for both roles requires
the reader to inspect the expression after `for` to determine which kind of loop they are
looking at.

```ryo
# Current: reader must parse the expression to know this is condition-based
for buffer.has_data():
	chunk = buffer.read(1024)
	process(chunk)

# With while: intent is visible at the keyword level
while buffer.has_data():
	chunk = buffer.read(1024)
	process(chunk)
```

In code review (Ryo's AI-writes, human-reviews workflow), the human scanning a function
can see `while` and immediately know the loop's control mechanism without reading the
expression. This matters when the condition is a function call or compound expression
that looks similar to an iterable:

```ryo
# Ambiguous at a glance -- is `socket.active()` an iterable or a boolean?
for socket.active():
	data = socket.read()
	handle(data)

# Unambiguous -- the keyword tells you it is a condition
while socket.active():
	data = socket.read()
	handle(data)
```

### Pro 2: Python Developer Expectations

Ryo's target audience is Python developers. Every Python developer has written:

```python
# Python
while True:
    line = input()
    if line == "quit":
        break
    process(line)

while not done:
    step()
```

Python's `while` is one of the first control-flow constructs taught in every course
and tutorial. When a Python developer sees `for condition:` in Ryo, they need to
unlearn an association (`for` = iteration) that they have held since their first week
of programming.

Ryo already borrows heavily from Python: colons, indentation, f-strings, `range()`,
`for item in iterable:`. Adding `while` would make another construct instantly
recognizable. Omitting it is a departure that needs strong justification -- but the
justification ("fewer keywords") saves exactly one keyword while creating friction
for every new user.

```ryo
# A Python developer's first Ryo program -- current syntax
mut guess = ""
for guess != secret:
	guess = input("Enter guess: ")
print("Correct!")

# Same program with while -- zero learning curve for this construct
mut guess = ""
while guess != secret:
	guess = input("Enter guess: ")
print("Correct!")
```

### Pro 3: Clearer Teaching and Documentation

With two keywords, documentation can explain loops as two distinct concepts:

- **`for`** iterates over things (collections, ranges).
- **`while`** repeats based on a condition.

This maps to how programmers (and AI assistants) think about loops. The current
design requires explaining that `for` has a third, non-obvious form where the
expression is a boolean rather than an iterable:

```ryo
# Teaching with while -- each keyword has one clear purpose
# "for iterates, while repeats"
for item in items:
	print(item)

while not done:
	step()
```

Teaching materials for the current design need a caveat:

> "`for` in Ryo has three forms. Two iterate over values. The third takes a boolean
> condition and repeats while that condition is true."

This third form is consistently the one that requires extra explanation. A `while`
keyword eliminates the caveat entirely.

---

## 3 Cons of Adding `while` (Arguments for Keeping `for condition:`)

### Con 1: Go Proved One Loop Keyword Works at Scale

Go has exactly one loop keyword (`for`) and uses it for all loop forms:

```go
// Go -- one keyword, four forms
for i := 0; i < 10; i++ { }    // C-style
for _, v := range items { }     // iteration
for condition { }               // condition-based
for { }                         // infinite
```

Go is used by millions of developers in production, powers critical infrastructure
(Docker, Kubernetes, Terraform), and no serious criticism of the language targets
its loop design. Go developers do not miss `while`. The unified `for` is considered
a feature, not a limitation.

Ryo's design philosophy explicitly cites Go's "fewer features, done well" approach.
Adding `while` moves away from this precedent: it introduces a second keyword that
does something the first keyword already handles.

```ryo
# Current Ryo -- same economy as Go, one keyword
for item in items:
	print(item)
for counter < 10:
	counter += 1
for true:
	serve()
```

### Con 2: Fewer Keywords Means a Simpler Language

Every keyword added to a language is a permanent commitment. `while` would be the
only keyword in Ryo that is fully expressible with another existing keyword --
it adds syntax without adding capability.

Ryo's keyword budget is a design choice. The language intentionally omits features
that other languages include (no exceptions, no operator overloading, no inheritance).
Each omission is a bet that simplicity is worth the unfamiliarity cost.

Adding `while` sets a precedent: if one convenience keyword is added, what about
`unless`, `until`, `loop`, `repeat`? The current rule is clean -- "one loop keyword"
-- and easy to defend. A two-keyword rule requires a new justification for where
the line is drawn.

```ryo
# One keyword -- the rule is simple and absolute
for item in items:
	print(item)
for i in range(10):
	print(i)
for count < limit:
	count += 1
for true:
	serve()
```

### Con 3: The Current Design Is Already Implemented and Documented

The `for condition:` syntax appears in:

- **specification.md** (lines 288-289, 303-306, 322) -- three separate locations
- **docs/CLAUDE.md** (line 35) -- project instructions for AI
- **docs/dev/implementation_roadmap.md** (line 718) -- milestone examples
- **docs/examples/** -- at least one example file (`advanced.ryo`, line 33)

Adding `while` requires updating all of these locations, revising the spec rationale,
updating the getting-started guide, modifying AI context documents, and deciding
whether `for condition:` remains valid (two ways to do the same thing) or is removed
(a breaking change to existing documentation and examples).

If `for condition:` is kept alongside `while`, the language has two ways to write
the same loop -- violating the "one obvious way" principle. If `for condition:` is
removed, all existing examples and documentation need rewriting, and the "three forms
of for" narrative (which is clean and well-documented) becomes "two forms of for
plus while."

---

## Evidence from Other Languages

### Python

Python has both `for` and `while` as separate keywords. `for` is exclusively for
iteration (`for x in iterable`). `while` is exclusively for condition-based
repetition. Python developers never confuse the two because the keywords signal
intent. This is the mental model Ryo's target audience carries.

### Go

Go has only `for`. It serves as the C-style loop, range-based iterator, condition
loop, and infinite loop. Go chose this deliberately in the name of simplicity.
The Go team's rationale: "Do less. Enable more." Go is the only widely-adopted
modern language that unifies all loops under one keyword.

Notably, Go uses braces and semicolons -- its `for condition { }` is visually
distinct from `for i := 0; i < n; i++ { }` because of the structural
difference. In Ryo's Python-style syntax, the visual distinction between
`for item in items:` and `for condition:` is subtler: the only cue is the
absence of `in`.

### Rust

Rust has `for`, `while`, and `loop` (three keywords). `for` iterates over
iterators. `while` handles conditions. `loop` is an explicit infinite loop.
Rust's philosophy: each loop form has a dedicated keyword, and the compiler
can reason about each form differently (e.g., `loop` guarantees at least one
iteration for type checking purposes).

### Swift

Swift has `for-in`, `while`, and `repeat-while`. Like Rust, each loop form
has its own keyword. Swift removed the C-style `for(;;)` loop in Swift 3,
judging it redundant, but kept `while` -- the Swift team considered `while`
to be a fundamentally different concept from iteration.

### Zig

Zig has both `for` (range-based iteration only) and `while` (condition-based,
including C-style `while (cond) : (afterthought)` forms). Zig, despite being
one of the most minimalist modern languages, chose to keep both keywords.
Andrew Kelley's design philosophy emphasizes "readability is paramount" -- the
two keywords exist because they communicate different intent to the reader.

### Summary Table

| Language | `for` | `while` | `loop` | Unified under `for`? |
|----------|-------|---------|--------|----------------------|
| Python   | Yes   | Yes     | No     | No                   |
| Go       | Yes   | No      | No     | **Yes**              |
| Rust     | Yes   | Yes     | Yes    | No                   |
| Swift    | Yes   | Yes     | No     | No                   |
| Zig      | Yes   | Yes     | No     | No                   |

Go is the sole outlier. Every other language in Ryo's reference set uses separate
keywords for iteration and condition-based loops.

---

## Recommendation

Ryo should add a `while` keyword. The case rests on audience, readability, and
precedent. Ryo's target audience is Python developers, and `while` is one of the
first constructs every Python developer learns -- omitting it creates unnecessary
friction for the exact people the language is designed to attract. Ryo's
"readable by default" principle (drawn from Zig) argues that the keyword itself
should signal intent: `for` means iteration, `while` means condition. In the
AI-writes, human-reviews workflow, a human scanning code should be able to
distinguish loop types at the keyword level without reading the expression --
`while` makes that possible, `for condition:` does not. Go is the only modern
language that successfully unified loops under `for`, and Go's brace-and-semicolon
syntax provides stronger visual cues to distinguish loop forms than Ryo's
colon-and-indentation syntax does. The cost of adding `while` is exactly one
keyword; the benefit is immediate recognition by every Python, Rust, Swift, and
Zig developer who encounters the language. If Ryo's design philosophy is
"Python's readability with Rust's safety and Go's simplicity," then `while`
belongs on the Python side of that ledger.

---

## Open Questions for Human Review

1. **Should `for condition:` remain valid alongside `while`?** Two ways to write
   the same loop is a code-style divergence risk, but removing it is a larger
   spec change.

2. **What syntax for infinite loops?** If `while` is added, should infinite loops
   be `while true:` (Python-style), or should Ryo add `loop:` (Rust-style) as
   well? Adding `loop:` is a separate question with its own tradeoffs.

3. **Does this affect `break`/`continue` semantics?** No -- both keywords work
   identically regardless of whether the loop is `for` or `while`.

---

*This memo is an input to a human decision. Tasks 14 and 15 are gated on the
outcome. No spec edits should be made until the human has reviewed and decided.*
