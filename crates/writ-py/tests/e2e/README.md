# Writ E2E Test Suite

End-to-end terminal automation tests for writ. Automates the manual pre-beta checklist so every check runs headless in CI and visually via tmux on your laptop.

## What This Tests

| File | Section | Coverage |
|------|---------|----------|
| `test_s1_golden_path.py` | S1: Golden Path | init, seal, context, status, spec lifecycle, finish round-trip |
| `test_s2_workspaces.py` | S2: Workspaces | create, isolation, convergence, cleanup, golden path workflow |
| `test_s5_enforcement.py` | S5: Enforcement | pre-commit hook, agent identity auto-detect, seal enforcement |
| `test_s6_edge_cases.py` | S6: Edge Cases | empty projects, unicode, concurrency, performance, error recovery |
| `test_live_agent.py` | S1.6/S5.5: Live | real Claude Code sessions, adoption scoring (opt-in) |

## Prerequisites

1. **writ binary built**: `cargo build --release -p writ-cli`
2. **Python venv active**: `source .venv/bin/activate`
3. **pexpect installed**: `pip install pexpect`
4. **claude CLI** (optional, for live tests only): must be on PATH

## Quickstart

```bash
source .venv/bin/activate && pytest crates/writ-py/tests/e2e/tests/ -v --tb=short
```

That's it. 60 tests, ~26 seconds. Watch the green checks fly by.

## How to Run

### Quick (default, CI safe)

```bash
# From repo root, venv activated
pytest crates/writ-py/tests/e2e/tests/ -v --tb=short
```

Runs all scripted tests. Live agent tests are skipped unless `--live` is passed.

### Run a single section

```bash
pytest crates/writ-py/tests/e2e/tests/test_s1_golden_path.py -v    # Golden path only
pytest crates/writ-py/tests/e2e/tests/test_s2_workspaces.py -v      # Workspaces only
pytest crates/writ-py/tests/e2e/tests/test_s5_enforcement.py -v     # Enforcement only
pytest crates/writ-py/tests/e2e/tests/test_s6_edge_cases.py -v      # Edge cases only
```

### Include slow tests (concurrency, performance)

```bash
pytest crates/writ-py/tests/e2e/tests/ -v -m "not live"
```

### Visual mode (tmux, the cool one)

```bash
./crates/writ-py/tests/e2e/run_visual.sh                  # All scripted tests
./crates/writ-py/tests/e2e/run_visual.sh --section s1     # Golden path only
./crates/writ-py/tests/e2e/run_visual.sh --live           # Include live agents

# Then attach to watch
tmux attach -t writ-e2e
```

Creates a tmux session with tests streaming in one pane and results in another.

### Headless mode (CI)

```bash
./crates/writ-py/tests/e2e/run_headless.sh                # Skip slow + live
./crates/writ-py/tests/e2e/run_headless.sh --all          # Include slow tests
```

### Live agent tests (launches real Claude Code)

```bash
pytest crates/writ-py/tests/e2e/tests/test_live_agent.py -v --live --timeout=600
```

Spins up a real `claude -p` session, gives it a zero writ prompt, and scores whether it discovers and adopts writ on its own using the 100 point AgentScorecard. Pass threshold: 70/100. Requires `claude` CLI on PATH. Each test takes 60-300s.

### After any run, check the report

```bash
cat crates/writ-py/tests/e2e/results/latest.json | python3 -m json.tool
```

## Results

After each run, a JSON report is written to `results/`:

- `latest.json` — always overwritten with most recent run
- `e2e_YYYYMMDDTHHMMSS.json` — timestamped copy for history

```bash
# View latest results
cat crates/writ-py/tests/e2e/results/latest.json | python3 -m json.tool

# Check pass/fail counts
python3 -c "import json; r=json.load(open('crates/writ-py/tests/e2e/results/latest.json')); print(f'P:{r[\"passed\"]} F:{r[\"failed\"]} S:{r[\"skipped\"]}')"
```

## Architecture

```
crates/writ-py/tests/e2e/
├── conftest.py          # Fixtures + JSON reporter
├── helpers/
│   ├── cli.py           # Re-exports from testing/roundtrip/helpers.py
│   └── interactive.py   # pexpect wrapper for interactive commands
├── tests/               # Test files (one per section)
├── results/             # JSON reports (.gitignored via .gitkeep)
├── run_visual.sh        # tmux launcher
└── run_headless.sh      # CI runner
```

**Key design**: helpers are imported from `testing/roundtrip/` (not duplicated). The `AgentScorecard`, `writ_cmd`, `writ_context`, scaffolds, and prompts all come from the existing roundtrip test infrastructure.

## Adding New Tests

1. Pick the right file based on section (S1-S6) or create a new `test_sN_*.py`
2. Use fixtures from `conftest.py`: `writ_project`, `writ_project_with_spec`, `portfolio_project`
3. Use helpers from `helpers/cli.py`: `writ_cmd`, `writ_context`, `writ_log`, etc.
4. Mark slow tests with `@pytest.mark.slow`
5. Mark live agent tests with `@pytest.mark.live`

## Debugging Failures

1. **Check the JSON report**: `cat crates/writ-py/tests/e2e/results/latest.json | python3 -m json.tool`
2. **Run single test with full output**: `pytest crates/writ-py/tests/e2e/tests/test_s1_golden_path.py::TestS1Init::test_writ_dir_created -v -s`
3. **Check writ binary is built**: `ls -la target/release/writ`
4. **Check venv**: `which python && python -c "import pexpect"`
5. **tmux debug**: `tmux attach -t writ-e2e` to watch live output

## Relationship to Other Test Layers

- **L1 (Rust unit tests)**: `cargo test` — tests internal logic
- **L2 (Scenario builder)**: `testing/scenarios/` — YAML-driven convergence scenarios
- **L3 (Python contract tests)**: `crates/writ-py/tests/` — Python binding contracts
- **L4 (YAML scenarios)**: `testing/scenarios/*.yaml` — declarative multi-agent scenarios
- **L5 (Live TRs)**: `crates/writ-py/tests/test_runs/` — scripted + live agent test runs
- **E2E (this suite)**: Automates the manual pre-beta checklist end-to-end
