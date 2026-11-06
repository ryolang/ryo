# Ryo F-String Implementation Specification

**Status:** Active implementation specification  
**Language target:** F-strings  
**Deferred:** T-strings  
**Deferred but preferred for reusable templates:** Compiled template definitions  
**Baseline compiler revision:** `3afa57c004396cd97b11c4f53fd693b2c7c02ec5`  
**Suggested repository location:** `docs/dev/f_strings.md`

## 1. Purpose

Add Python-style formatted string literals to Ryo:

```ryo
fn main():
	name = "Alice"
	score = 95
	message = f"Hello {name}, score: {score}"
	print(message)
	print(name)
```

Expected output:

```text
Hello Alice, score: 95
Alice
```

An f-string evaluates its interpolations eagerly, converts supported values to text, and returns a standard owning `str`.

This specification also records two independent future directions:

1. **T-strings are deferred.** If implemented, they should produce a non-escaping `TemplateView`, not an owning captured-value `Template`.
2. **Compiled templates are deferred but are the preferred reusable-template facility.** They must not depend on t-strings or on the f-string runtime lowering.

## 2. Design decisions

| Topic | Decision |
|---|---|
| F-string result | Owning `str` |
| Evaluation | Eager, exactly once, left to right |
| Interpolation scope | Ordinary lexical expression scope |
| Initial supported values | `str`, `strview`, `bytes`, `bytesview`, `int`, `float`, `bool`, structs, anonymous structs, and enums |
| Interpolated `str` lvalues | Read/borrow for formatting; not moved merely because they appear in an f-string |
| Temporary values | Normal Ryo temporary ownership and destruction rules |
| Escaped braces | `{{` and `}}` |
| Nested expressions | Use the normal expression lexer/parser, including nested delimiters |
| Format conversions | Deferred: no `!s`, `!r`, or `!a` |
| Format specifications | Deferred: no `:.2f`, `:04`, or nested format fields |
| Debug specifier | Deferred: no `{value=}` |
| Raw/triple-quoted f-strings | Deferred |
| Bytes formatting | Use the existing escaped `bytes` representation; no implicit UTF-8 decoding |
| User-defined formatting | Custom `Display` implementations are deferred; existing compiler-synthesized `DebugRepr` formatting is supported |
| T-strings | Not implemented or made public by this milestone |
| Compiled/runtime templates | Not implemented by this milestone |

### 2.1 Why f-strings return `str`

A dynamic formatting result needs storage. Returning `strview` would require a hidden owner and would make common operations such as returning or storing an f-string result invalid. An owning `str` is consistent with Ryo's existing move semantics and small-string optimization.

### 2.2 Why f-strings do not require t-strings

F-strings and t-strings can share interpolation scanning and parsing machinery, but they have different value and ownership semantics. F-string implementation must not construct a public template object and must not wait for a template type, renderer trait, variadic generics, or runtime polymorphism.

### 2.3 Why compiled templates are separate

A compiled template definition stores reusable structure and accepts its context at render time. An f-string or t-string evaluates values at the literal site. These are different abstractions and must remain independently implementable.

### 2.4 Milestone placement

Basic f-string interpolation ships as **M17 — F-Strings & String Interpolation** in v0.1/alpha, after M16 Optional Types and before M24 Method Implementations. The execution order is **M13 → M13.8 → M16 → M17 → M24**. M17 requires no traits, t-strings, or template machinery.

**M22 — Static Format Specifications** follows M20 Explicit Sizes and M21 `char`. M20 and M21 extend plain interpolation and the existing default conversion paths to their new types. M22 adds static precision, width, alignment, fill, alternate-form, and numeric-base specifications across the complete numeric and character type system. It must not duplicate or redesign the plain conversion paths. Text width counts Unicode scalar values, not UTF-8 bytes, grapheme clusters, or terminal display columns.

Conversions and `{value=}` ship in **M23**, after the static format-specification work. Dynamic width or precision and custom `Display` formatting remain v0.2 work. T-strings and compiled templates remain deferred under Sections 11 and 12.

## 3. Non-goals

This milestone does not attempt to provide:

- safe SQL or safe HTML merely through string interpolation;
- a SQL query builder or parameter-binding API;
- an HTML component tree or autoescaping renderer;
- reusable template definitions;
- template inheritance, includes, macros, or template-file loading;
- runtime parsing of user-supplied templates;
- lazy interpolation or closures capturing interpolation expressions;
- an owning captured-value `Template`;
- a public `TemplateView` API;
- variadic functions or heterogeneous runtime value lists;
- a general runtime formatting framework.

Documentation must explicitly warn that f-string interpolation does not make SQL, HTML, shell commands, URLs, or other structured languages safe.

## 4. Surface syntax

### 4.1 Informal grammar

```text
f-string       := 'f"' f-item* '"'
f-item         := text | interpolation | '{{' | '}}'
interpolation  := '{' expression '}'
```

The prefix must be immediately adjacent to the opening quote. The initial implementation supports lowercase `f` and the same single-line, double-quoted literal form as ordinary Ryo strings.

Examples:

```ryo
f""
f"plain text"
f"value: {value}"
f"{left} + {right} = {left + right}"
f"literal braces: {{ and }}"
f"nested {f"{value}"}"
```

The following are outside the initial syntax:

```text
F"value"             uppercase prefix
rf"value\n"          raw f-string
f"value: {x:.2f}"   format specification
f"value: {x!r}"     conversion
f"value: {x=}"      debug specifier
fb"value"           bytes interpolation
```

`t"..."` remains unsupported. A dedicated “t-strings are not implemented” diagnostic is permitted, but no t-string behavior is required.

### 4.2 Text chunks

Text chunks use the ordinary string escape decoder and diagnostics. Standard escapes such as `\n`, `\r`, `\t`, `\\`, `\"`, `\xHH`, and `\u{...}` retain their existing meaning. In particular, `f"Hello\n{name}"` is supported: `\n` is decoded to a line-feed character in the resulting `str`; it is not treated as a physical source newline. Newline characters contained in interpolated values are also preserved without restriction.

Before decoding text, the scanner applies these rules in order:

1. Consume the raw extent of an ordinary string escape, including any braces in a `\u{...}` escape.
2. Recognize `{{` and `}}` as escaped brace pairs.
3. Recognize a single `{` as the start of an interpolation.
4. Reject a single `}` outside an interpolation.

The ordinary escape decoder validates the consumed escape later. A malformed escape such as `f"\u{ZZ}"` produces its ordinary Unicode diagnostic without secondary brace or interpolation errors. Braces produced by decoding are always literal text:

```ryo
f"\u{1F600}"     # "😀"
f"\u{7B}\u{7D}" # "{}"
f"\x7b\x7d"     # "{}"
```

Escaped brace pairs append one literal brace to the current text chunk. Backslash escaping does not replace brace doubling.

Doubled braces are recognized only in text mode. Expression mode uses normal delimiter balancing. Therefore, an opening escaped pair takes precedence over the combination of an interpolation opener and an anonymous struct opener:

```ryo
f"{{x=1}}"      # literal text: {x=1}
f"{({x=1})}"    # interpolated anonymous struct
f"{ {x=1} }"    # also valid
```

Parentheses are the canonical spelling for brace-form anonymous struct interpolation in documentation.

Adjacent text content must be coalesced where practical. Empty text chunks need not be emitted except that an entirely empty f-string still produces an empty `str`.

### 4.3 Interpolation expressions

An interpolation contains one ordinary Ryo expression. The implementation must reuse the normal expression parser rather than creating a second expression grammar.

The scanner must handle nested constructs correctly, including:

- parentheses and calls;
- brackets and slices;
- braces belonging to struct literals;
- ordinary string literals inside an interpolation;
- escapes and nested f-strings;
- nested t-string syntax only sufficiently to issue the normal unsupported-feature diagnostic, not to implement it.

A scanner that simply searches for the next `}` is incorrect. For example, the parser must delimit this expression correctly and semantic analysis must format the resulting struct through its existing `DebugRepr`:

```ryo
f"{Point{x=1, y=2}}"
```

Statements and assignments are not interpolation expressions unless the ordinary expression grammar already accepts the construct.

### 4.4 Initial restrictions

The initial implementation may reject:

- physical, unescaped line breaks inside an f-string literal (escaped `\n` and `\r` remain supported);
- triple-quoted forms;
- uppercase or combined prefixes;
- top-level conversion and format-specifier syntax.

These restrictions should produce targeted diagnostics where practical rather than generic lexer corruption or parser recovery cascades.

## 5. Semantics

### 5.1 Result and conversion rules

The result type is always `str` after successful semantic analysis. M17 uses a compiler-provided default conversion and performs no trait lookup.

| Interpolated type | Required behavior |
|---|---|
| `str` | Append its contents without changing them; do not move an lvalue solely because it was interpolated |
| `strview` | Append the viewed contents without changing them; preserve view validity during formatting |
| `int` | Use exactly the semantics of `int_to_str` |
| `float` | Use exactly the semantics of `float_to_str` |
| `bool` | Use exactly the semantics of `bool_to_str` |
| struct | Use the existing compiler-synthesized `DebugRepr`, matching `print(value)` |
| anonymous struct | Use its existing structural `DebugRepr`, including positional/paren and named/brace forms |
| enum | Use its existing compiler-synthesized `DebugRepr`, including recursive payload formatting |
| `bytes`, `bytesview` | Use the existing escaped bytes representation from `print(value)`, lowered through `__ryo_bytes_repr`; do not decode as UTF-8 |
| `void`, optionals, and values without an available conversion | Compile-time type error |
| error type | Preserve normal error recovery and avoid cascading diagnostics |

For `int`, `float`, `bool`, structs, anonymous structs, and enums, semantic lowering should reuse the existing `DebugRepr` path used by `print`. This avoids creating a second aggregate-formatting policy and automatically preserves recursive nested-struct/enum behavior. `DebugRepr` borrows its operand and returns an owning `str`, so formatting must not move the interpolated aggregate lvalue. For `bytes` and `bytesview`, reuse the corresponding `__ryo_bytes_repr` path so printable ASCII, standard escapes, and `\xNN` output exactly match `print(value)`.

The formatting conversion must use compiler-internal dispatch or the existing builtin implementation paths. It must not perform ordinary user-function lookup that could be changed by shadowing a helper named `int_to_str`, `float_to_str`, or `bool_to_str`. `print(value)` keeps its current conversion rules and does not switch to `Display`.

#### Custom `Display` precedence

When custom `Display` implementations arrive, interpolation uses this selection order:

1. Use a visible `Display` implementation for a user-defined nominal type.
2. Otherwise, use the built-in default conversion when available.
3. Otherwise, emit a compile-time type error at the interpolation expression.

Built-in type formatting is fixed and cannot be overridden by `Display`. Anonymous structs always use structural `DebugRepr` because they have no nominal type for an implementation. A named struct or enum can override its own default representation through `Display`.

Default `DebugRepr` recursion always uses `DebugRepr` for fields and payloads. A field's `Display` implementation does not change the default representation of an enclosing aggregate. If the enclosing type has no default representation, the conversion is rejected even when a field has `Display`.

The selected conversion runs exactly once and borrows its operand. Existing output remains stable until a named aggregate receives an explicit `Display` implementation.

### 5.2 Evaluation order

Interpolations are evaluated exactly once, from left to right, at the point where the f-string expression is evaluated.

Given conceptually:

```ryo
f"{first()}{second()}{third()}"
```

the required call order is:

```text
first → second → third → completion of the result string
```

The compiler may choose when to assemble literal chunks around those calls, but it must not reorder observable expression evaluation.

There is no lazy evaluation and no implicit closure creation.

### 5.3 Ownership

The f-string result is an independent owning value and may be returned, stored, moved, or freed like any other `str`.

For an interpolated lvalue:

- a `str` is borrowed/read for formatting;
- a `strview` remains subject to the existing non-escaping view rules;
- scalar values follow their ordinary copy/read semantics;
- structs, anonymous structs, and enums are borrowed by `DebugRepr` while their representation is produced;
- interpolation alone must not make the original `str` or aggregate binding unavailable after the f-string.

For an interpolated rvalue:

- evaluate it normally;
- keep required storage alive until formatting completes;
- free or destroy temporary owners according to existing ownership rules.

Moving the left side of each concatenated intermediate is consistent with the existing `str + str` rule: move left, borrow right. User-visible `str` interpolations must not become accidental left-owner moves merely because of compiler-generated concatenation.

### 5.4 Runtime behavior

Formatting inherits the runtime behavior of the selected conversion and string operations:

- integer formatting matches `int_to_str`, including `i64::MIN`;
- float formatting matches `float_to_str`, including non-finite values;
- Boolean formatting is `true` or `false`;
- struct, anonymous-struct, and enum formatting matches `print(value)` through `DebugRepr`, including recursive fields and payloads;
- `bytes` and `bytesview` formatting matches `print(value)` through `__ryo_bytes_repr`, including printable ASCII, short escapes, and `\xNN` escapes;
- Unicode text is preserved as UTF-8;
- allocation failure behavior is unchanged from existing string operations;
- no additional panics are introduced for supported values.

## 6. Compiler architecture

### 6.1 Required pipeline property

Do not desugar an f-string directly into source text before parsing. Do not split it into independent string and expression tokens using a regular expression alone.

The required pipeline is:

```text
source
  → interpolation-aware literal scanner
  → normal tokens and normal expression parser for each hole
  → segmented AST/UIR representation
  → type-directed semantic lowering
  → existing TIR string/conversion operations
  → ownership analysis
  → existing backend/runtime
```

A segmented representation must survive until semantic analysis because the correct conversion depends on each expression's inferred type.

### 6.2 Shared interpolation scanner

Add a scanner whose responsibility is specifically to delimit interpolated literal text and expression spans. It should be parameterized by literal kind internally where that is useful, but only f-strings become public in this milestone.

The scanner should produce conceptual spans such as:

```text
Text { source_span }
Expression { source_span }
LiteralBrace { character, source_span }
```

It must cooperate with the ordinary string escape decoder and preserve absolute source offsets.

A suitable implementation may use a raw f-string token followed by sub-lexing/sub-parsing each expression, or a modal lexer that emits equivalent segmented tokens. The implementation choice is open, but the following invariants are mandatory:

- normal expression lexing is reused;
- nested delimiters are balanced;
- quotes inside an expression do not terminate the outer literal;
- nested interpolated literals do not corrupt the outer scanner state;
- diagnostics use spans in the original source file;
- no synthetic source string becomes the primary source of user-facing spans.

Avoid naming this module after a milestone or implementation phase. A responsibility-oriented name such as `interpolation.rs` is appropriate if a new module is needed.

### 6.3 Token representation

The current plain string path produces one `StrLit(StringId)`. F-strings need a distinct token or token sequence that preserves their segmented/raw nature until parsing.

A raw-body token is acceptable:

```text
FString(raw_body_or_source_handle)
```

The token must not contain only the final decoded text, because that would lose expression boundaries and source locations.

### 6.4 AST representation

Add a segmented expression representation rather than treating an f-string as `Literal::Str`.

Conceptually:

```text
InterpolatedString {
    parts: InterpolatedPartList,
}

InterpolatedPart:
    Text {
        value: StringId,
        span: Span,
    }
    Expression {
        expression: ExprId,
        span: Span,
    }
```

Use side arenas and `Copy` list handles where required by the existing AST design. Expression nodes remain ordinary `ExprId` values and continue to use the existing expression arena.

Do not add public t-string variants, template-value types, conversion fields, or format-specifier fields until their respective milestones require them. Forward compatibility should come from reusable scanning and a clean segmented representation, not speculative public language features.

AST pretty-printing and driver AST dumps must show the literal chunks and interpolation expressions clearly enough for debugging.

### 6.5 UIR and semantic analysis

The AST-to-UIR stage must preserve the segmented form. A dedicated internal interpolated-string instruction/node is preferable to an early source-like desugaring into user-visible calls.

Semantic analysis must:

1. Analyze each expression in source order using the normal expression path.
2. Read its inferred type.
3. Emit the appropriate internal formatting conversion or reject unsupported types.
4. Combine literal chunks and converted values into an owning `str`.
5. Preserve ownership and view information needed by the existing ownership pass.

The initial lowering may use existing `StrConcat`, conversion builtins, and view materialization operations where required. Later it may use an internal builder based on `str_push` to reduce intermediate allocations. Such an optimization must preserve evaluation order, spans, and ownership behavior.

The lowering must not expose compiler-generated bindings or helper names to user lookup.

### 6.6 Backend and runtime

No new backend feature is inherently required for the first implementation if existing conversion, concatenation, materialization, and destruction operations are sufficient.

Prefer reusing:

- `int_to_str` semantics and code generation;
- `float_to_str` semantics and code generation;
- `bool_to_str` semantics and code generation;
- the existing `DebugRepr` paths for scalars, structs, anonymous structs, and enums;
- the existing `__ryo_bytes_repr` path for `bytes` and `bytesview`;
- `StrConcat` and its ownership behavior;
- existing `str`/`strview` handling;
- existing SSO and destruction optimizations.

A specialized f-string runtime ABI or a variadic runtime call is explicitly out of scope.

## 7. Diagnostics

Diagnostics must point into the original f-string, including inside interpolation expressions.

Required error cases include:

- unterminated f-string;
- unterminated interpolation;
- empty interpolation `{}`;
- unmatched closing brace;
- mismatched nested delimiters;
- invalid expression inside an interpolation;
- invalid string escape in a text chunk;
- unsupported conversion or format-specifier syntax;
- unsupported interpolated type;
- nested unsupported literal forms where a targeted diagnostic is practical.

Example type diagnostic:

```text
error: cannot interpolate value of type '?int' in an f-string
  --> example.ryo:3:18
   |
 3 |     message = f"{maybe}"
   |                  ^^^^^
   |
   = supported values: str, strview, bytes, bytesview, int, float, bool, structs, anonymous structs, and enums
```

The exact wording may follow existing diagnostic conventions. Error recovery should avoid reporting a long cascade for one unmatched delimiter and should continue parsing subsequent statements when safe.

## 8. Testing requirements

### 8.1 Scanner and lexer tests

Cover:

- empty and plain-text f-strings;
- one and multiple interpolations;
- adjacent interpolations;
- `{{` and `}}` in multiple positions, including beside interpolation holes;
- ordinary string escapes and Unicode;
- raw braces inside `\u{...}` escapes and decoded braces from `\u{7B}`, `\u{7D}`, `\x7b`, and `\x7d` remaining literal text;
- malformed Unicode escapes producing one escape diagnostic without secondary brace or interpolation errors;
- doubled braces in text mode versus balanced delimiters in expression mode;
- nested parentheses, brackets, and struct-literal braces;
- strings and escaped quotes inside holes;
- nested f-strings;
- unterminated literals and holes;
- empty holes and unmatched braces;
- absolute spans after source prefix removal.

### 8.2 Grammar and AST tests

Verify:

- holes use the ordinary expression grammar;
- operator precedence inside holes is unchanged;
- `f"{{x=1}}"` produces literal text, while `f"{({x=1})}"` and `f"{ {x=1} }"` interpolate anonymous structs;
- nested anonymous structs preserve their expression boundaries;
- nested calls, slices, field accesses, and binary expressions parse correctly;
- AST side arenas contain the expected parts in source order;
- AST pretty output and debug dumps include interpolated strings;
- malformed input recovers without panics.

### 8.3 Semantic tests

Verify every supported type and representative unsupported types:

- `str` remains usable after interpolation;
- `strview` contents are formatted without invalidating the owner;
- integer, float, and Boolean output matches their existing conversion helpers;
- struct, anonymous-struct, and enum output matches `print(value)` and uses `DebugRepr`;
- nested structs, recursive enum payloads, positional anonymous structs, and named anonymous structs render correctly;
- interpolated aggregate lvalues remain usable after formatting;
- `bytes` and `bytesview` output matches `print(value)`, including non-UTF-8 and non-printable bytes;
- unsupported values produce one targeted type error;
- helper-name shadowing cannot change formatting semantics;
- expressions are evaluated exactly once and left to right;
- errors inside holes retain normal semantic diagnostics;
- an f-string result can be returned from a function and stored in a variable.

### 8.4 Ownership and runtime tests

Run compile-and-execute tests for:

- empty output and literal-only output;
- mixed supported types;
- long strings exceeding SSO capacity;
- Unicode and multibyte boundaries;
- interpolated temporary owners, including aggregate values;
- nested struct/enum formatting and destruction of their temporary debug strings;
- interpolated slices/views, including `bytesview`;
- binary inputs containing NUL, invalid UTF-8, printable ASCII, and every byte value from `0x00` through `0xff`;
- repeated use of the original `str` after formatting;
- nested f-strings;
- results returned across function boundaries;
- both JIT and AOT paths where the existing integration harness supports them.

Run the existing ASan/Valgrind coverage relevant to string ownership. No leaks, double frees, use-after-move, or stale views are acceptable.

### 8.5 Performance checks

Add or extend a focused benchmark only if the implementation introduces a materially new code-generation path. At minimum:

- check for pathological compile-time growth with many holes;
- compare repeated concatenation with a builder-based optimization if one is introduced;
- verify that small results still benefit from existing SSO behavior;
- do not claim zero allocation or linear-time assembly unless measured.

## 9. Documentation and repository updates

When the feature ships:

1. Update `docs/specification.md` with the implemented syntax, supported types, ownership behavior, and deferred formatting features.
2. Update `docs/dev/implementation_roadmap.md` to reflect the shipped subset and remaining work.
3. Reconcile the stale statement in `docs/dev/proposals.md` that says basic f-strings already exist.
4. Add `examples/f_strings.ryo` using only implemented behavior.
5. Update `landing/reference/index.html`: remove the planned callout and remove f-strings from “What's Next.”
6. Verify the playground/highlighter handles interpolation expressions and escaped braces correctly.
7. Verify ownership documentation still matches actual behavior.

Documentation examples must use Ryo's mandatory tab indentation and Python-style blocks.

## 10. Implementation slices

### Slice 1: Scanning and parsing

- Add the interpolation-aware scanner.
- Add raw/segmented f-string tokens.
- Parse holes with the normal expression parser.
- Add the segmented AST/UIR representation and debug output.
- Complete malformed-input tests before enabling execution.

### Slice 2: Scalar and aggregate semantic lowering

- Support `str`, `int`, `float`, and `bool`.
- Support structs, anonymous structs, and enums through the existing `DebugRepr` path.
- Support `bytes` and `bytesview` through the existing `__ryo_bytes_repr` path.
- Emit type-directed conversions and string assembly.
- Add semantic and initial integration tests, including nested aggregates and borrowed operands.

### Slice 3: Views and ownership hardening

- Support `strview` without invalid escapes or stale owners.
- Verify temporary destruction and non-move behavior for interpolated `str` lvalues.
- Run JIT, AOT, ASan, and Valgrind coverage.

### Slice 4: Diagnostics, documentation, and optimization review

- Complete span and recovery tests.
- Update all language-surface documentation and examples.
- Profile many-part f-strings before deciding whether an internal `str_push` builder is justified.

Each slice must keep the compiler buildable and must not expose unfinished t-string behavior.

## 11. Deferred t-string direction

T-strings are not part of this implementation. Their distinct purpose would be to let libraries process inline interpolated literals before producing a result. Unlike an f-string, a t-string would preserve static text and evaluated values instead of combining them immediately. This is an ergonomic and library-extensibility facility, not an automatic safety or reusable-template facility.

The preferred future direction, if t-strings are added, is:

```text
t"..." → non-escaping TemplateView
```

A `TemplateView` would support direct, synchronous processing such as `render(t"<p>{name}</p>")`. It could not be stored, returned, or passed beyond the lifetime of its captured owners. It therefore would not provide Python's full owning template-object model.

The intended properties are:

- eager evaluation, exactly once, left to right;
- static literal chunks plus captured value views;
- expression source spans and conversion or format metadata preserved for processors when those features exist;
- anonymous structural shapes with static dispatch;
- no required enums, variadics, or runtime type erasure;
- local storage and passing downward into synchronous consumers;
- rejection when a view escapes its captured owners;
- consumer-defined escaping, binding, validation, and result construction;
- consumers produce owning domain results such as `Html`, `str`, or a bound query.

The processing library owns any SQL, HTML, logging, or other domain-safety guarantee. The literal syntax alone provides no such guarantee.

F-string work may reuse the interpolation scanner with this future feature, but must not expose `TemplateView`, reserve a public runtime ABI, or implement an owning captured `Template`. M17 does not expose processor metadata or change its runtime lowering to support t-strings.

## 12. Deferred compiled-template direction

Compiled templates are also deferred, but they are the preferred solution when Ryo needs reusable external templates.

The distinction is:

```text
Interpolated literal: structure + values evaluated now
Compiled template:    reusable structure + context supplied later
```

A compiled template should contain no views into values captured at its definition site. It may therefore be stored or made globally available without changing `TemplateView` semantics or adding an owning captured-value `Template`.

The preferred future capabilities are:

- external template files;
- typed contexts;
- compile-time expression checking;
- parse-once/render-many behavior;
- includes and inheritance where justified;
- shared HTML escaping and output-writer infrastructure;
- ordinary functions for reusable inline components.

A Go-style runtime template parser remains a separate, lower-priority facility for user-editable or dynamically loaded source. Neither compiled nor runtime templates should be made dependencies of f-strings or t-strings.

## 13. Acceptance criteria

The f-string milestone is complete when:

- the documented syntax and supported types work in JIT and AOT compilation;
- result ownership and interpolation non-move behavior match this specification;
- evaluation is exactly once and left to right;
- nested delimiters and nested f-strings parse correctly;
- malformed literals produce accurate diagnostics without compiler panics;
- ownership, SSO, ASan, and Valgrind tests show no regression;
- the workspace test suite passes;
- formatting and lint checks pass;
- all language-surface documentation agrees with the implementation;
- no public t-string or compiled-template functionality is required for the f-string tests to pass.

Suggested final checks:

```bash
cargo fmt --check
RUSTFLAGS=-Dwarnings cargo clippy --workspace --all-targets
cargo test --workspace
./scripts/check_file_length.sh
./scripts/run_linux_tests.sh
```

## References

- Spec: [Section 2](../specification.md) (string and f-string literals), [Section 6.1.2](../specification.md) (interpolation as the non-variadic printing idiom)
- Dev: [implementation_roadmap.md](implementation_roadmap.md) (M17, M22, and M23)
- Dev: [compiler_guide.md](compiler_guide.md) (pipeline, AST/UIR, semantic-analysis, backend, and testing conventions)
