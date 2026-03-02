# Installation

Writ can be installed through several package managers. Choose whichever fits your toolchain.

## Python (PyPI)

The fastest way to get started. Installs both the CLI and the Python SDK.

```bash
pip install writ-vcs
```

After installation, the `writ` command is available in your terminal and `import writ` works in Python.

## Rust (Cargo)

Build the CLI from source. Requires the Rust toolchain.

```bash
cargo install --path crates/writ-cli
```

Or, once published to crates.io:

```bash
cargo install writ
```

## macOS (Homebrew)

Coming soon. Once the Homebrew tap is published:

```bash
brew install writ
```

## From Source

Clone the repository and build:

```bash
git clone https://github.com/andrew-garfield101/writ.git
cd writ
cargo build --release
```

The binary will be at `target/release/writ`. Add it to your PATH or copy it somewhere convenient.

For the Python bindings:

```bash
cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin
maturin develop
```

## Verify Installation

```bash
writ --version
# writ 0.1.0

writ --help
# Shows all available commands
```

## Next Steps

Head to the [Quickstart](quickstart.md) to set up writ in a project and create your first seal.
