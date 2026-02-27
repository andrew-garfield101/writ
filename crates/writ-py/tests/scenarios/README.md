# YAML Scenario Tests

Declarative convergence test scenarios executed by a headless runner against real writ repositories.

## Running

```bash
# All scenarios
pytest tests/scenarios/ -v

# Single category
pytest tests/scenarios/convergence/ -v

# Single scenario
pytest tests/scenarios/convergence/disjoint_files.yaml -v
```

## Directory structure

```
scenarios/
├── assertions/          # Assertion dispatch modules
│   ├── convergence.py   # Convergence + verification assertions
│   ├── metadata.py      # Repo state assertions (spec_exists, seal_count)
│   └── security.py      # Chain integrity assertions
├── convergence/         # Core convergence scenarios
├── e2e/                 # End-to-end multi-concern scenarios
├── scale/               # Scale testing (5-agent, 10-agent)
├── security/            # Seal chain integrity scenarios
├── conftest.py          # Pytest auto-discovery for .yaml files
└── runner.py            # ScenarioRunner execution engine
```

## Scenario format

```yaml
scenario: my_test            # Unique name (required)
version: 1
description: >               # What this tests
  ...
tags: [convergence, conflict]

setup:
  specs:                      # Spec definitions
    - id: spec-a
      file_scope: ["src/*.py"]  # Optional scope constraint
  baseline:                   # Files present before agents run
    - path: shared.py
      content: |
        original

agents:
  - id: agent-a
    spec: spec-a
    changes:
      - path: shared.py
        action: write         # write | append | delete
        content: |
          modified

convergence:
  strategy: escalate          # escalate | three-way-merge | most-complete
  apply: true                 # Default true; set false for dry-run

assertions:
  convergence:                # Post-merge file/report checks
    - type: not_degraded
    - type: file_contains
      file: shared.py
      content: modified
  verification:               # Syntax and traceability checks
    - type: syntax_valid
      file: shared.py
  security:                   # Chain integrity checks
    - type: chain_valid
  metadata:                   # Repo state checks
    - type: spec_exists
      spec_id: spec-a
```

## Available assertion types

### Convergence
| Type | Fields | Description |
|------|--------|-------------|
| `no_escalations` | — | No files escalated to human review |
| `has_escalations` | — | At least one escalation present |
| `escalation_count` | `expected` | Exact number of escalations |
| `is_clean` | `expected` (bool) | Convergence report is_clean field |
| `not_degraded` | — | No quality degradation |
| `all_definitions_preserved` | `file`, `definitions` (list) | All named definitions present in file |
| `file_contains` | `file`, `content` | File contains substring |
| `file_not_contains` | `file`, `content` | File does NOT contain substring |
| `file_exists` | `file` | File exists on disk |
| `file_deleted` | `file` | File does NOT exist on disk |
| `files_changed_count` | `expected` | Number of files changed |
| `confidence_above` | `file`, `threshold`, `require_report` (opt) | Confidence score above threshold |
| `total_conflicts` | `expected` | Exact conflict count |
| `total_auto_merged_gte` | `minimum` | At least N auto-merged files |
| `escalated_file` | `file` | Specific file was escalated |

### Verification
| Type | Fields | Description |
|------|--------|-------------|
| `syntax_valid` | `file` | Python: compiles; others: exists and non-empty |
| `no_silent_additions` | — | No untracked additions in traceability report |

### Security
| Type | Fields | Description |
|------|--------|-------------|
| `chain_valid` | — | Full chain verification passes |
| `chain_seal_count` | `expected` | Number of seals in chain |
| `seal_has_content_hash` | — | All seals have content_hash |
| `seal_has_chain_hash` | — | All seals have chain_hash |
| `chain_no_failures` | — | No verification failures in chain |

### Metadata
| Type | Fields | Description |
|------|--------|-------------|
| `spec_exists` | `spec_id` | Spec is registered in repo |
| `context_has_field` | `field` | Context output contains named field |
| `diverged_branches_count` | `expected` | Number of diverged branches |
| `seal_count` | `expected` | Total seals in HEAD chain (includes convergence seals) |

## Known limitations

- **Sequential-seal runner**: Agents execute sequentially, not in parallel. Baseline is restored before each agent to simulate parallel work. This means file deletions by agent A get restored before agent B's seal captures the tree state.
- **Syntax checking**: Only Python files are compiled; other languages get existence + non-empty checks only.
