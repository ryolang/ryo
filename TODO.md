# TODO

- [ ] review https://docs.modular.com/mojo/manual/values/ownership copy design
- [ ] get the domain https://ryolang.org
- [ ] Add https://scorecard.dev/
- [ ] Set up clippy https://github.com/rust-lang/rust-clippy
- [ ] set up https://codspeed.io/
- [ ] review for more https://github.com/astral-sh/ruff/tree/main/.github/workflows

### Core language
- Lexer: Lexical, Syntactic Grammar
  1. Arithmetic
  2. Basic types
  3. Operators
  4. Comments
     1. Document Comments
  5. print
  6. Control Flow: if, for
  7. Functions
  8. Variables
  9. Imports
  10. Enums
      1. match
  11. Errors
      1. `?` for error propagation
  12. Structs
  13. Traits
- Parser
- AST
- Interpreter (AST visitor)
- REPL
- Basic Compiler
- Basic Mem Management
- Testing - go to: lexer next number
### Safety & Concurrency
- Borrow & Ownership
- SCP concurrency
- STD
- Testing
  

## Resources

- https://www.python-httpx.org/
- https://mlir.llvm.org/docs/Dialects/
- https://edgl.dev/blog/mlir-with-rust/
- https://github.com/edg-l/edlang
- https://github.com/ehsanmok/create-your-own-lang-with-rust/tree/master
- https://createlang.rs
- https://pest.rs/book/
- https://doc.rust-lang.org/rust-by-example
- https://buzz-lang.dev/guide/
- https://github.com/rust-lang/rustlings/
- https://github.com/zesterer/tao
- https://course.ccs.neu.edu/cs4410sp19/lec_type-inference_notes.html
- https://rustc-dev-guide.rust-lang.org/type-inference.html
- https://github.com/rhaiscript/rhai

```shell
cargo run -- "1+1"
```

```shell
objdump -d output.o
objdump -h output.o
objdump -x output.o
```

```shell
otool -tV output.o
otool -h output.o
otool -l output.o
```

```shell
nm output.o
```

```shell
xxd output.o | less
hexdump -C output.o | less
```

## experimental deps
* melior = "0.19.0"
* pest = "2.7.14"
* pest_derive = "2.7.14"
* lalrpop = "0.22"
* lalrpop-util = "0.22"
* lazy_static = "1.5.0"

* async to sync https://github.com/zesterer/pollster