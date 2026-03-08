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

## Removing Writ

Writ has two layers: **per project** setup and the **system install**. Remove either or both depending on what you need.

### Remove writ from a project

```bash
writ uninit --force
```

This removes `.writ/`, framework hooks (CLAUDE.md markers, slash commands, settings.json permissions), and the `.gitignore` entry. Your source code, git history, and other config are never touched.

Use `--keep-writignore` if you want to preserve your `.writignore` file.

### Remove writ from your system

Depends on how you installed it:

```bash
# PyPI
pip uninstall writ-vcs -y

# Cargo
cargo uninstall writ

# Homebrew (when available)
brew uninstall writ

# From source — remove the binary you added to PATH
rm /path/to/writ
```

After uninstalling, optionally remove global config:

```bash
rm -rf ~/.writ/
```

This is a small config directory. Removing it is safe and has no effect on your projects or git history.

## Next Steps

Head to the [Quickstart](quickstart.md) to set up writ in a project and create your first seal.
