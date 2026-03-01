# Contributing to Writ

Thank you for your interest in contributing to writ. This document covers everything you need to get started.

## Development Setup

### Prerequisites

- **Rust toolchain** (stable, 1.75+): [rustup.rs](https://rustup.rs/)
- **Python 3.10+** with `venv` support
- **maturin** for building Python bindings
- **git** for version control

### Clone and Build

```bash
git clone https://github.com/andrew-garfield101/writ.git
cd writ

# Build the Rust core and CLI
cargo build

# Run Rust tests
cargo test -p writ-core -p writ-cli
```

### Python Bindings

```bash
cd crates/writ-py
python3 -m venv .venv
source .venv/bin/activate
pip install maturin pytest pyyaml
maturin develop

# Run Python tests
cd ../..
python -m pytest crates/writ-py/tests/ -v
```

### YAML Scenario Tests

```bash
source .venv/bin/activate
python -m pytest testing/scenarios/ -v
```

## Test Commands

| What | Command |
|------|---------|
| Rust core tests | `cargo test -p writ-core` |
| CLI tests | `cargo test -p writ-cli` |
| Bridge tests | `cargo test -p writ-core --all-features` |
| Python tests | `python -m pytest crates/writ-py/tests/ -v` |
| YAML scenarios | `python -m pytest testing/scenarios/ -v` |
| All Rust | `cargo test -p writ-core -p writ-cli` |

## Code Style

### Rust

- Run `cargo fmt` before committing
- No regex crate. Use `contains_word()` for word boundary matching.
- Follow standard Rust conventions (snake_case functions, PascalCase types)

### Python

- Follow PEP 8
- Use type hints for function signatures
- Use pytest for all tests

## Commit Conventions

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add token refresh endpoint
fix: handle empty seal summary
refactor: extract convergence pipeline into module
test: add three-agent convergence scenario
docs: update quickstart guide
chore: bump dependency versions
```

Keep commits atomic. One logical change per commit. Don't commit commented out code or debug logs.

## Pull Request Process

1. Fork the repository and create a feature branch
2. Make your changes with tests
3. Run the full test suite (Rust + Python)
4. Run `cargo fmt` to format Rust code
5. Submit a PR with a clear description of what changed and why

### PR Description Template

```markdown
## Summary
Brief description of the change.

## Test Plan
How was this tested? Include test output if relevant.
```

## Project Structure

```
writ/
├── crates/
│   ├── writ-core/          # Core library (Rust)
│   ├── writ-cli/           # CLI binary (clap)
│   └── writ-py/            # Python bindings (PyO3)
├── book/                   # Documentation site (mdbook)
├── testing/                # YAML scenarios, test runs
└── docs/                   # Internal planning docs
```

### Key Files

- `crates/writ-core/src/repo.rs` is the central file. All operations flow through the `Repository` struct.
- `crates/writ-core/src/convergence/` contains the six-phase convergence engine.
- `crates/writ-cli/src/main.rs` has all CLI command definitions.
- `crates/writ-py/src/lib.rs` has the Python bindings.

## Reporting Bugs

File issues at [github.com/andrew-garfield101/writ/issues](https://github.com/andrew-garfield101/writ/issues). Include:

- What you expected to happen
- What actually happened
- Steps to reproduce
- Writ version (`writ --version`)
- OS and architecture

## Contributor License Agreement

By submitting a pull request, you agree to license your contribution under the same license as the project (AGPL-3.0). We require a CLA for all external contributions to ensure the project can be dual-licensed in the future.

## Questions?

Open a discussion or issue on GitHub. We're happy to help contributors get oriented.
