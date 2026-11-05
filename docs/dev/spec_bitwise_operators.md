**Status:** Design (v0.1) — approved 2026-11-05

# Bitwise Operators

**Direction:** use the conventional operator spellings from Rust, Go, and Python ("be boring"). Ratified 2026-11-05; originated in a 2026-10-28 design session.

## 1. Syntax

| Operation | Operator | Example |
|---|---|---|
| Bitwise AND | `&` | `a & b` |
| Bitwise OR | `\|` | `a \| b` |
| Bitwise XOR | `^` | `a ^ b` |
| Bitwise NOT | `~` | `~a` |
| Left shift | `<<` | `a << 2` |
| Right shift | `>>` | `a >> 2` |

Logical operators remain unchanged: `and`, `or`, `not`.

Compound assignment uses the conventional forms:

```ryo
x &= mask
x |= flags
x ^= value
x <<= 1
x >>= 1
```

## 2. Operator roles

The specification changes from "each operator has exactly one meaning" to **"each operator has one meaning per syntactic role."**

### Prefix `&`: mutable borrow

```ryo
increment(&count)
```

### Infix `&`: bitwise AND

```ryo
masked = value & 0xff
```

The parser distinguishes these by position.

### Type-position `|`: error-union composition

```ryo
fn load() -> (NotFound | InvalidData)!str:
```

### Expression-position `|`: bitwise OR

```ryo
flags = read | write
```

The parser also distinguishes these by position.

## 3. Consequence: no bare-`|` pipe

Expression-position `|` is bitwise OR under the one-meaning-per-syntactic-role rule, so a bare-`|` collection pipe is not available in Ryo:

```ryo
# Not Ryo: `|` is bitwise OR in expressions
items | fn(x): x * 2
```

Collection pipelines stay on the iterator-chain and free-function forms. This is the main language-design cost of conventional bitwise operators, and it was accepted at ratification (2026-11-05).

## 4. Type rules

Initial implementation supports the existing signed `int` type:

- `&`, `|`, `^`: both operands must have the same type, and the result is that type.
- `<<`, `>>`: the left operand is the shifted integer value; the shift count is `int`. The result has the value's type.
- `~` returns the operand type.
- No implicit conversions between numeric types.
- `bool`, floats, strings, and aggregates are rejected.

Milestone 20 (explicit sizes) extends the operators to sized integers:

- Signed and unsigned integers support all six operators.
- Mixed sized types still require explicit conversion.
- The result type follows the left operand.

## 5. Runtime semantics

Bit operations have fixed-width, two's-complement semantics. They do not use arithmetic overflow checks.

```ryo
~0 == -1
0xff & 0x0f == 0x0f
0x01 << 3 == 8
```

### Left shift

`<<` shifts bits toward the most-significant end. Bits beyond the integer width are discarded.

This is a bit operation, not checked multiplication. For example, shifting into the sign bit produces the corresponding signed bit pattern rather than an arithmetic-overflow panic.

### Right shift

`>>` is:

- **Arithmetic for signed integers**, preserving the sign bit.
- **Logical for unsigned integers** after M20, inserting zero bits.

Thus, for signed `int`:

```ryo
-8 >> 1 == -4
```

### Invalid shift counts

A shift count panics when it is:

- Negative.
- Greater than or equal to the operand's bit width.

The compiler rejects invalid constant counts at compile time.

## 6. Precedence

Use conventional Rust/Python-shaped precedence, from tighter to looser:

```text
unary -, not, ~
* / %
+ -
<< >>
&
^
|
ordering
equality
and
or
```

Therefore:

```ryo
value & mask == 0
```

means:

```ryo
(value & mask) == 0
```

Bitwise operators are left-associative at their respective levels.

## 7. Examples

```ryo
fn is_set(value: int, mask: int) -> bool:
	return value & mask != 0

fn set_flag(inout value: int, flag: int):
	value |= flag

fn clear_flag(inout value: int, flag: int):
	value &= ~flag

fn main():
	mut flags = 0
	set_flag(&flags, 0x04)
	set_flag(&flags, 0x02)

	print(int_to_str(flags))       # 6
	print(int_to_str(flags << 1))  # 12

	clear_flag(&flags, 0x04)
	print(int_to_str(flags))       # 2
```

## 8. Implementation outline

1. Add operator tokens and precedence levels.
2. Add AST, UIR, and TIR representations.
3. Add semantic checks for integer operands and shift counts.
4. Lower to Cranelift `band`, `bor`, `bxor`, `bnot`, `ishl`, `sshr`, and later `ushr`.
5. Add compound-assignment lowering.
6. Test precedence, negative values, sign shifts, discarded bits, invalid counts, and `INT_MIN`.

No ownership or runtime-library changes are required.

## Decision

Adopted 2026-11-05:

```text
& | ^ ~ << >>
```

with conventional compound assignments and Rust/Python-shaped precedence.

The accepted tradeoff: **bare `|` becomes bitwise OR in expressions, and the bare-`|` collection-pipe line is abandoned** (the pipe proposal's analysis is kept on record with its status marked abandoned).

## References

- Spec: Section 4.2 (explicit sizes — the sized-type extension), Section 18 (traps in all build modes — the invalid-shift-count rule)
- Spec: the operator-uniqueness rule ("each operator has exactly one meaning") — this proposal amends it to one-meaning-per-syntactic-role
- Roadmap: [Milestone 19 — Bit Operations](implementation_roadmap.md#milestone-19-bit-operations)
