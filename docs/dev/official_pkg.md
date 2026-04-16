# Official Packages

For a language targeting **Web Backends** and **CLI Tools**, three critical gaps remain after `sqlite`, `json`, and `uuid`.

For developers to actually *ship* products with Ryo v0.2, the following official packages are needed as **Native Extensions** (wrapping Rust crates).

---

### 1. The CLI Essential: `pkg:cli` (Argument Parsing)
Serious CLI tools cannot be built by manually parsing `os.args()` (a list of strings). Flags, help generation, and subcommands are required.

*   **The Rust Backing:** **`clap`** (or `lexopt` for lighter weight).
*   **Why `clap`?** It is the industry standard. It auto-generates beautiful `--help` messages and handles edge cases (`-xvf` expansion).
*   **The Ryo DX:**
    Instead of Rust's complex builder pattern, a declarative struct mapping is offered (similar to how `serde` maps JSON).

    ```ryo
    import cli
    
    struct Args:
        @cli.arg(short="p", long="port", help="The port to listen on")
        port: int = 8080
        
        @cli.arg(help="The file path")
        path: str

    fn main():
        # Parses os.args() and fills the struct
        args = cli.parse[Args]() 
        print(f"Listening on {args.port}")
    ```

---

### 2. The Backend Essential: `pkg:postgres`
SQLite works for dev, but production backends use PostgreSQL. Without Postgres support at launch, Ryo risks being dismissed as a toy by backend engineers.

*   **The Rust Backing:** **`rust-postgres`** (synchronous/blocking for v0.1/0.2).
*   **Why?** Pure Rust (easy to compile/link via Zig), fast, and secure.
*   **The Ryo DX:**
    The API design should match the `sqlite` package. Switching databases should require only changing the import and connection string.

    ```ryo
    import postgres
    
    fn main():
        conn = try postgres.connect("postgres://user:pass@localhost/db")
        rows = try conn.query("SELECT * FROM users WHERE id = $1", [42])
    ```

---

### 3. The Modern Essential: `pkg:dotenv`
In modern development, nobody hardcodes configuration. `.env` files and Environment Variables are standard, following the "12-Factor App" methodology for web services.

*   **The Rust Backing:** **`dotenvy`**.
*   **Why?** It loads a `.env` file into the process environment variables instantly on startup.
*   **The Ryo DX:**
    
    ```ryo
    import dotenv
    import std.env

    fn main():
        dotenv.load() # Finds .env and loads it
        
        # Now use standard env
        key = env.get("API_KEY") orelse panic("No key found")
    ```

---

### 4. The "Nice to Have": `pkg:image`
Since Ryo targets **Data Processing**, users will expect image operations (resize, convert PNG to JPEG, crop thumbnails) without needing Python/OpenCV.

*   **The Rust Backing:** **`image`** (The Rust Image Project).
*   **Why?** Pure Rust (no external C lib dependencies like libpng/libjpeg), making it trivial to cross-compile via `ryo build`.
*   **The Ryo DX:**
    ```ryo
    import image
    
    fn main():
        img = image.open("photo.png")
        thumb = img.resize(100, 100)
        thumb.save("thumb.jpg")
    ```

---

### Summary of the "Official Suite"

The **Official Packages** list (managed by the core team) for a credible ecosystem launch:

| Package | Domain | Rust Backing | Priority |
| :--- | :--- | :--- | :--- |
| `pkg:sqlite` | DB / Dev | `rusqlite` | **Critical** |
| `pkg:postgres` | DB / Prod | `rust-postgres` | **Critical** |
| `pkg:cli` | CLI Tools | `clap` | **High** |
| `pkg:dotenv` | Config | `dotenvy` | **High** |
| `pkg:http_server` | Web | `tiny_http` (v0.2) | **High** |
| `pkg:image` | Data | `image` | Medium |
| `pkg:zip` | Utils | `zip` | Low |

**Strategic Note:** These should not be placed in the Standard Library (`std`). Keeping `std` small (I/O, FS, Net, Core) and putting these in the registry allows updating the Postgres driver version without forcing users to upgrade their Ryo Compiler.
