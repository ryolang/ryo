# TODO

- [ ] Iterators
- [ ] Define Testing
- [ ] https://ziglang.org/documentation/0.1.1/#case-study-printf
- [ ] Debug Breakpoints https://ziglang.org/documentation/0.1.1/#builtin-breakpoint
- [ ] main function required?
- [ ] my_str: str = "hello" when pass as reference fn world(hello: str) ->, should not include the derref world(&str), python and new developers do not understand &, we should use a short english word as we do for && -> and
- [ ] Zen of ryo. https://dave.cheney.net/2020/02/23/the-zen-of-go
- [ ] review https://docs.modular.com/mojo/manual/values/ownership copy design
- [ ] Set up benchmarks for performance and memory in github workflows and keep track of them.
- [ ] get the domain https://ryo-lang.org
- [ ] Add https://scorecard.dev/
- [ ] Set up clippy https://github.com/rust-lang/rust-clippy
- [ ] set up https://codspeed.io/
- [ ] review for more https://github.com/astral-sh/ruff/tree/main/.github/workflows
- [ ] Effective Go. review and get insigths https://go.dev/doc/effective_go
- [ ] Integration with Notebooks [Jupytext](https://jupytext.readthedocs.io/en/latest/formats-scripts.html)
- [ ] Include integration for [Tilelang IR](https://github.com/tile-ai/tilelang)

## Resources

**Language Design**:
- [Language Design](https://cs.lmu.edu/~ray/notes/languagedesignnotes/)
- [Grammar visualizer](https://ohmjs.org/editor/)
- [Mojo Manual - Ownership](https://docs.modular.com/mojo/manual/values/ownership)
- [Mojo Ownership Notebook](https://github.com/modularml/mojo/blob/main/docs/manual/values/ownership.ipynb)
- [Mojo mental model](https://www.modular.com/blog/what-ownership-is-really-about-a-mental-model-approach)
- [Mojo Ownership deep dive](https://www.modular.com/blog/deep-dive-into-ownership-in-mojo)
- [Type Inference Course](https://course.ccs.neu.edu/cs4410sp19/lec_type-inference_notes.html)
- [Rustc Dev Guide - Type Inference](https://rustc-dev-guide.rust-lang.org/type-inference.html)
- [Pony Tutorial - Actors](https://tutorial.ponylang.io/types/actors) - Concurrency via actors
- [Lattner's Concurrency Gist](https://gist.github.com/lattner/31ed37682ef1576b16bca1432ea9f782)

**Implementation Guides**:
- [Create Your Own Language with Rust](https://github.com/ehsanmok/create-your-own-lang-with-rust/tree/master)
- [createlang.rs](https://createlang.rs)
- [Rust by Example](https://doc.rust-lang.org/rust-by-example)
- [Pest Parser Book](https://pest.rs/book/)

**Compiler Infrastructure**:
- [MLIR Documentation](https://mlir.llvm.org/docs/Dialects/)
- [MLIR with Rust](https://edgl.dev/blog/mlir-with-rust/)
- [Melior - MLIR bindings for Rust](https://github.com/mlir-rs/melior)
- [Compiler Explorer](https://github.com/compiler-explorer/compiler-explorer)

**Concurrency Runtimes**:
- [Smol - Small async runtime](https://github.com/smol-rs/smol)
- [Pollster - Minimal executor](https://github.com/zesterer/pollster)
- [Zig's I/O discussion](https://kristoff.it/blog/zig-new-async-io/) - Function colorblindness
- [Go concurrency patterns](https://go.dev/tour/concurrency/) - Goroutines and channels

**Performance & Benchmarking**:
- [Mojo Benchmark Guide](https://mojodojo.dev/guides/std/Benchmark.html)
- [Fibonacci: Python vs Mojo vs Rust](https://www.statpan.com/2023/10/python-vs-mojo-vs-rust-fibonacci-speed.html)

**Similar Projects**:
- [Tao Language](https://github.com/zesterer/tao)
- [Buzz Language](https://buzz-lang.dev/guide/)
- [edlang](https://github.com/edg-l/edlang)

**Learning Resources**:
- [Rustlings - Rust exercises](https://github.com/rust-lang/rustlings/)

**Libraries & Tools**:
- [Rhai - Embedded scripting for Rust](https://github.com/rhaiscript/rhai)
- [Wild - Fast linker](https://github.com/davidlattimore/wild)
- [HTTPX - Python HTTP client](https://www.python-httpx.org/) - Reference for HTTP library design
- [CRISP Code Principles](https://bitfieldconsulting.com/posts/crisp-code) - Correct, Readable, Idiomatic, Simple, Performant

**Community Resources**:
- [Mojo Dojo](https://mojodojo.dev/)
