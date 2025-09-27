# Ryo Package Manager

Let's design a package manager for Ryo, taking inspiration from both Cargo and Go Modules, but tailored to Ryo's philosophy of being Python-like yet safe and performant. We'll lean more towards the Cargo style for its robustness, but aim for some Go Modules-inspired simplicity where possible.

## Overview

**Name:** Ryo Package Manager is integrated into **`ryo`** binary.

**Manifest File:** We'll use a TOML-based manifest file, like Cargo's `Cargo.toml`, for its structured and readable format. Let's name it **`ryo.toml`**.

**Registry:** We will establish a **central public registry** for Ryo packages, let's call it **`ryopkgs.io`**. This provides discoverability and a central point of trust for Ryo packages.

**Versioning:** We will enforce **Semantic Versioning (SemVer)** for Ryo packages. This is crucial for managing dependencies and ensuring compatibility.

**Dependency Resolution:** We'll implement a **robust dependency resolution algorithm**, drawing inspiration from Cargo's resolver. It should handle version conflicts and select compatible versions, aiming for a "newest compatible" strategy by default, similar to Cargo.

## CLI Commands

The `ryo` tool will be the primary way users interact with the package manager. Here are the essential subcommands:

### Project Management

#### `ryo new <project_name>`

Create a new Ryo project.

**Action:** Creates a new directory named `<project_name>`.

**Content:**
- Inside `<project_name>`:
  - `ryo.toml`: A basic manifest file pre-filled with project name and essential sections.
  - `src/`: Source code directory.
    - `src/main.ryo`: A basic "Hello, World!" program in Ryo.

**Example:** `ryo new hello_ryo`

```toml
# ryo.toml (inside hello_ryo directory)
[package]
name = "hello_ryo"
version = "0.1.0"
authors = ["Your Name <your.email@example.com>"]
edition = "2024" # Or specify Ryo edition if versions are introduced

[dependencies]
# Dependencies will be listed here
```

#### `ryo new --lib <project_name>`

Create a new Ryo library.

**Action:** Creates a new directory named `<project_name>`.

**Content:**
- Inside `<project_name>`:
  - `ryo.toml`: A basic manifest file pre-filled with project name and essential sections.
  - `src/`: Source code directory.
    - `src/lib.ryo`: A basic library template in Ryo.

**Example:** `ryo new --lib lib_ryo`

### Dependency Management

#### `ryo add <package_name> [<version_constraint>]`

Add a dependency to the current project.

**Action:** Modifies the `ryo.toml` file in the current directory to add a dependency.

**Arguments:**
- `<package_name>`: The name of the Ryo package to add (e.g., `ryo-utils`, `fast-http`).
- `[<version_constraint>]` (Optional): Version requirement (e.g., `=1.2.3`, `^1.0`, `~2.0`). If omitted, it defaults to the latest version.

**Behavior:**
- Adds the dependency to the `[dependencies]` section in `ryo.toml`.
- If no version constraint is provided, it will fetch the latest version from `ryopkgs.io`.

**Examples:**
- `ryo add ryo-utils` (adds latest version)
- `ryo add fast-http ^0.3` (adds version compatible with 0.3.x)

```toml
# ryo.toml (after `ryo add ryo-utils`)
[package]
name = "hello_ryo"
version = "0.1.0"
authors = ["Your Name <your.email@example.com>"]
edition = "2024"

[dependencies]
ryo-utils = "1.0" # Latest version resolved and added
```

#### `ryo install`

Install project dependencies.

**Action:** Resolves dependencies listed in `ryo.toml`, downloads them from `ryopkgs.io`, and prepares them for building.

**Behavior:**
- Reads `ryo.toml` to get dependencies.
- Resolves dependency versions based on constraints and the registry.
- Downloads required packages and their dependencies to a local cache (`~/.ryo-cache` or similar).
- Creates or updates a `ryo.lock` file in the project directory. This `ryo.lock` file records the exact versions of all direct and transitive dependencies that were resolved in this install.

**`ryo.lock` File:** The `ryo.lock` file is crucial for **repeatable builds**. It ensures that everyone working on the project, and in deployment environments, uses the *exact same versions* of dependencies, preventing "works on my machine" issues due to dependency version mismatches.

#### `ryo update`

Update dependencies.

**Action:** Updates dependencies of the current project to their latest compatible versions, respecting version constraints in `ryo.toml`.

**Behavior:**
- Re-resolves dependencies based on `ryo.toml`.
- Downloads newer versions (within constraints) if available.
- Updates `ryo.lock` to reflect the new resolved versions.

#### `ryo lock`

Generate or refresh the `ryo.lock` file without updating dependencies.

**Action:** Ensures the `ryo.lock` file is up-to-date with the currently resolved dependency versions, without actively seeking newer versions (unlike `ryo update`).

**Use Case:** Useful for ensuring repeatable builds without changing dependency versions. Often used in CI/CD pipelines or when you want to strictly control dependency versions.

### Build and Execution

#### `ryo build`

Build the current project and its dependencies.

**Action:** Compiles the Ryo project and all its dependencies.

**Behavior:**
- Reads `ryo.toml` and `ryo.lock` (if it exists, for repeatable builds, otherwise resolves dependencies based on `ryo.toml` and creates/updates `ryo.lock`).
- Downloads any missing dependencies (if `ryo.lock` is not present or incomplete).
- Compiles all dependencies and then the current project's source code.
- Produces an executable (or library, depending on project type - initially focus on executables).
- Output executable is placed in a standard location (e.g., `target/debug/` for debug builds, `target/release/` for release builds - like Cargo).

#### `ryo run`

Run the main executable of the current project.

**Action:** Builds the project (if necessary) and then executes the resulting executable.

**Behavior:**
- If the project hasn't been built yet, it will implicitly call `ryo build` first.
- Executes the compiled executable, typically located in the debug build output directory (`target/debug/`).
- For running release builds, users should use `ryo build --release` and then execute the binary in `target/release/`.

#### `ryo test`

Run tests for the current project.

**Action:** Discovers and runs tests within the project.

**Behavior:**
- Compiles the project and its dependencies (similar to `ryo build`).
- Locates test functions (using a convention, e.g., functions annotated with `#[test]` attribute, similar to Rust or Python's `unittest`).
- Executes the tests and reports results (pass/fail).

### Publishing

#### `ryo publish`

Publish a package to `ryopkgs.io`.

**Action:** Packages and publishes the current project as a Ryo package to the central registry.

**Behavior:**
- Reads metadata from `ryo.toml` (package name, version, etc.).
- Packages the source code (and potentially pre-compiled libraries, assets, etc.).
- Uploads the package to `ryopkgs.io`.
- Requires user authentication/authorization on `ryopkgs.io` to prevent unauthorized publishing.

**Process:** Similar to `cargo publish` or `npm publish`.

### Task Runner

#### `ryo run <task>`

Run custom tasks defined in `ryo.toml`.

**Action:** Executes predefined tasks, similar to `make` or `just` commands.

**Configuration:** Tasks are defined in the `[tasks]` section of `ryo.toml`:

```toml
[tasks]
format = "ryo fmt src/"
lint = "ryo clippy --all"
docs = "ryo doc --open"
clean = "rm -rf target/"
dev = ["ryo build", "ryo run"]
```

**Examples:**
- `ryo run format` - Formats the source code
- `ryo run dev` - Runs multiple commands in sequence
- `ryo run clean` - Cleans build artifacts

## Implementation Notes

### CLI Tool Implementation

**Rust for `ryo` Implementation (Recommended):** Implementing `ryo` in Rust itself is a very good idea. Rust's performance, reliability, and existing crates for CLI tools, TOML parsing, networking, etc., would make it a highly suitable choice.

**Clear Error Messages:** `ryo` should provide user-friendly and informative error messages for common issues (e.g., dependency resolution failures, network errors, invalid `ryo.toml`, publishing errors).

**Help and Documentation:** Implement `ryo help` and help messages for each subcommand to guide users. Also, provide comprehensive online documentation for `ryo`.

### Registry Design (`ryopkgs.io`)

**Package Metadata Storage:** Database to store package metadata (name, version, description, authors, dependencies, download URLs, etc.).

**Package File Storage:** Storage for uploaded Ryo package files (likely `.tar.gz` or `.zip` archives containing source code and potentially pre-built binaries).

**Search and Discovery:** Implement search functionality on `ryopkgs.io` to allow users to easily find Ryo packages.

**User Accounts and Publishing Management:** User registration, login, API keys for publishing, package ownership management, version management, security measures to prevent malicious packages.

**Consider mirroring or proxying for robustness:** Potentially use a CDN for package downloads and consider proxying for dependencies hosted on other platforms (though initially, focus on Ryo-native packages in `ryopkgs.io`).

## Development Strategy

### Simplified vs. Feature-Rich

For the initial version of `ryo`, it's wise to start with a **simplified but functional core** set of features: `new`, `add`, `install`, `build`, `run`, `test`, `publish`. Focus on making these core commands robust and user-friendly. Then, iteratively add more advanced features like workspaces, more sophisticated build customization, advanced dependency resolution options, etc., based on user feedback and community needs.

## Conclusion

This Cargo-style `ryo` package manager design, with a central registry, TOML manifest, and a set of clear CLI commands, provides a strong and well-structured foundation for managing Ryo packages and projects. It balances robustness with relative simplicity for users, and sets the stage for a healthy and growing Ryo ecosystem.

The addition of task runner functionality (similar to `make` or `just`) provides developers with a convenient way to define and execute common development tasks directly through the `ryo` CLI, reducing the need for external build tools and scripts.