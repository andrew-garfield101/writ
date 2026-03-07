//! Python bindings for writ — AI-native version control.
//!
//! Exposes the full writ-core API to Python via PyO3.
//! Return types are Python dicts (via pythonize) for maximum
//! agent/LLM friendliness.

use std::path::PathBuf;

use pyo3::prelude::*;

use writ_core::context::{ContextFilter, ContextScope};
use writ_core::seal::{AgentIdentity, TaskStatus, Verification};
use writ_core::spec::{Spec, SpecUpdate};

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

pyo3::create_exception!(writ, WritError, pyo3::exceptions::PyException);

/// Convert a writ_core::WritError into a PyErr.
fn writ_err(err: writ_core::WritError) -> PyErr {
    WritError::new_err(err.to_string())
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[pyclass(name = "AgentType", eq, eq_int)]
#[derive(Clone, PartialEq)]
pub enum PyAgentType {
    Human = 0,
    Agent = 1,
}

#[pyclass(name = "TaskStatus", eq, eq_int)]
#[derive(Clone, PartialEq)]
pub enum PyTaskStatus {
    InProgress = 0,
    Complete = 1,
    Blocked = 2,
}

#[pyclass(name = "SpecStatus", eq, eq_int)]
#[derive(Clone, PartialEq)]
pub enum PySpecStatus {
    Pending = 0,
    InProgress = 1,
    Complete = 2,
    Blocked = 3,
}

// ---------------------------------------------------------------------------
// Enum conversion helpers
// ---------------------------------------------------------------------------

fn parse_agent_type(s: &str) -> PyResult<writ_core::seal::AgentType> {
    match s.to_lowercase().as_str() {
        "human" => Ok(writ_core::seal::AgentType::Human),
        "agent" => Ok(writ_core::seal::AgentType::Agent),
        other => Err(WritError::new_err(format!(
            "unknown agent type: '{other}' (expected 'human' or 'agent')"
        ))),
    }
}

fn parse_task_status(s: &str) -> PyResult<TaskStatus> {
    match s.to_lowercase().as_str() {
        "in-progress" | "inprogress" | "in_progress" => Ok(TaskStatus::InProgress),
        "complete" | "completed" => Ok(TaskStatus::Complete),
        "blocked" => Ok(TaskStatus::Blocked),
        other => Err(WritError::new_err(format!(
            "unknown task status: '{other}' (expected 'in-progress', 'complete', or 'blocked')"
        ))),
    }
}

fn parse_spec_status(s: &str) -> PyResult<writ_core::spec::SpecStatus> {
    s.parse::<writ_core::spec::SpecStatus>()
        .map_err(|e| WritError::new_err(e))
}

// ---------------------------------------------------------------------------
// Serde → Python dict helper
// ---------------------------------------------------------------------------

fn to_pydict<T: serde::Serialize + ?Sized>(py: Python, value: &T) -> PyResult<PyObject> {
    let obj = pythonize::pythonize(py, value).map_err(|e| WritError::new_err(e.to_string()))?;
    Ok(obj.unbind())
}

/// Format seals as a string using the given formatter, or return a Python dict.
fn format_seals(py: Python, seals: &[writ_core::seal::Seal], format: &str) -> PyResult<PyObject> {
    match format {
        "dict" => to_pydict(py, seals),
        "json" | "json-compact" | "toon" => {
            let formatter = writ_core::format::formatter_for(format)
                .ok_or_else(|| WritError::new_err(format!("unknown format: '{format}'")))?;
            let output = formatter
                .format_seal_log(seals)
                .map_err(|e| WritError::new_err(e.to_string()))?;
            Ok(pyo3::types::PyString::new(py, &output).into_any().unbind())
        }
        other => Err(WritError::new_err(format!(
            "unknown format: '{other}' (expected 'dict', 'json', 'json-compact', or 'toon')"
        ))),
    }
}

/// Format a diff output as a string using the given formatter, or return a Python dict.
fn format_diff(py: Python, diff: &writ_core::diff::DiffOutput, format: &str) -> PyResult<PyObject> {
    match format {
        "dict" => to_pydict(py, diff),
        "json" | "json-compact" | "toon" => {
            let formatter = writ_core::format::formatter_for(format)
                .ok_or_else(|| WritError::new_err(format!("unknown format: '{format}'")))?;
            let output = formatter
                .format_diff(diff)
                .map_err(|e| WritError::new_err(e.to_string()))?;
            Ok(pyo3::types::PyString::new(py, &output).into_any().unbind())
        }
        other => Err(WritError::new_err(format!(
            "unknown format: '{other}' (expected 'dict', 'json', 'json-compact', or 'toon')"
        ))),
    }
}

/// Format a status output as a string using the given formatter, or return a Python dict.
fn format_status(
    py: Python,
    status: &writ_core::status::StatusOutput,
    format: &str,
) -> PyResult<PyObject> {
    match format {
        "dict" => to_pydict(py, status),
        "json" | "json-compact" | "toon" => {
            let formatter = writ_core::format::formatter_for(format)
                .ok_or_else(|| WritError::new_err(format!("unknown format: '{format}'")))?;
            let output = formatter
                .format_status(status)
                .map_err(|e| WritError::new_err(e.to_string()))?;
            Ok(pyo3::types::PyString::new(py, &output).into_any().unbind())
        }
        other => Err(WritError::new_err(format!(
            "unknown format: '{other}' (expected 'dict', 'json', 'json-compact', or 'toon')"
        ))),
    }
}

/// Format specs as a string using the given formatter, or return a Python dict.
fn format_specs(py: Python, specs: &[writ_core::spec::Spec], format: &str) -> PyResult<PyObject> {
    match format {
        "dict" => to_pydict(py, specs),
        "json" | "json-compact" | "toon" => {
            let formatter = writ_core::format::formatter_for(format)
                .ok_or_else(|| WritError::new_err(format!("unknown format: '{format}'")))?;
            let output = formatter
                .format_spec_list(specs)
                .map_err(|e| WritError::new_err(e.to_string()))?;
            Ok(pyo3::types::PyString::new(py, &output).into_any().unbind())
        }
        other => Err(WritError::new_err(format!(
            "unknown format: '{other}' (expected 'dict', 'json', 'json-compact', or 'toon')"
        ))),
    }
}

#[derive(serde::Serialize)]
struct SealResult {
    #[serde(flatten)]
    seal: writ_core::seal::Seal,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflict_warning: Option<writ_core::repo::SealConflictWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_scope_warning: Option<writ_core::repo::FileScopeWarning>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hints: Vec<String>,
}

fn build_seal_result(
    repo: &writ_core::Repository,
    seal: writ_core::seal::Seal,
    conflict_warning: Option<writ_core::repo::SealConflictWarning>,
) -> SealResult {
    let file_scope_warning = seal.spec_id.as_ref().and_then(|sid| {
        let changed: Vec<String> = seal.changes.iter().map(|c| c.path.clone()).collect();
        repo.check_file_scope(sid, &changed)
    });

    let mut hints = Vec::new();

    if let Some(ref w) = file_scope_warning {
        hints.push(format!(
            "SCOPE: {} file(s) outside spec '{}' scope: {}",
            w.out_of_scope_files.len(),
            w.spec_id,
            w.out_of_scope_files
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    if seal.changes.is_empty() && !seal.summary.is_empty() {
        hints.push(
            "GHOST_WORK: 0 file changes detected but summary is non-empty. \
             Another agent may have sealed overlapping files first. \
             Check `writ context` for file ownership."
                .to_string(),
        );
    }

    SealResult {
        seal,
        conflict_warning,
        file_scope_warning,
        hints,
    }
}

// ---------------------------------------------------------------------------
// Repository wrapper
// ---------------------------------------------------------------------------

#[pyclass(name = "Repository")]
pub struct PyRepository {
    inner: writ_core::Repository,
}

#[pymethods]
impl PyRepository {
    /// Initialize a new writ repository.
    #[staticmethod]
    fn init(path: &str) -> PyResult<Self> {
        let p = PathBuf::from(path);
        let inner = writ_core::Repository::init(&p).map_err(writ_err)?;
        Ok(PyRepository { inner })
    }

    /// Open an existing writ repository.
    #[staticmethod]
    fn open(path: &str) -> PyResult<Self> {
        let p = PathBuf::from(path);
        let inner = writ_core::Repository::open(&p).map_err(writ_err)?;
        Ok(PyRepository { inner })
    }

    /// One-command setup: init + detect git + import baseline.
    #[staticmethod]
    fn install(py: Python, path: &str) -> PyResult<PyObject> {
        let p = PathBuf::from(path);
        let result = writ_core::Repository::init_project(&p).map_err(writ_err)?;
        to_pydict(py, &result)
    }

    /// Get working directory state as a dict.
    fn state(&self, py: Python) -> PyResult<PyObject> {
        let state = self.inner.state().map_err(writ_err)?;
        to_pydict(py, &state)
    }

    /// Create a seal from current changes.
    ///
    /// If `paths` is provided, only seal matching files (selective seal).
    /// Otherwise seals all changes.
    ///
    /// Automatic conflict detection: if `context()` was called before this
    /// seal, the HEAD recorded at that time is used to check whether another
    /// agent sealed in between. If so, the returned dict includes a
    /// `conflict_warning` field with details.
    #[pyo3(signature = (summary, agent_id="human", agent_type="human", spec_id=None, status="complete", paths=None, tests_passed=None, tests_failed=None, linted=false, allow_empty=false))]
    fn seal(
        &self,
        py: Python,
        summary: &str,
        agent_id: &str,
        agent_type: &str,
        spec_id: Option<String>,
        status: &str,
        paths: Option<Vec<String>>,
        tests_passed: Option<u32>,
        tests_failed: Option<u32>,
        linted: bool,
        allow_empty: bool,
    ) -> PyResult<PyObject> {
        let agent = AgentIdentity {
            id: agent_id.to_string(),
            agent_type: parse_agent_type(agent_type)?,
        };
        let task_status = parse_task_status(status)?;
        let verification = Verification {
            tests_passed,
            tests_failed,
            linted,
        };

        // Check if context() recorded a HEAD for automatic conflict detection.
        let tracked_head = self.inner.last_context_head();

        if let Some(ref p) = paths {
            let seal = self
                .inner
                .seal_paths(
                    agent,
                    summary.to_string(),
                    spec_id,
                    task_status,
                    verification,
                    p,
                    allow_empty,
                )
                .map_err(writ_err)?;
            self.inner.clear_context_head();
            let result = build_seal_result(&self.inner, seal, None);
            to_pydict(py, &result)
        } else if tracked_head.is_some() {
            let (seal, warning) = self
                .inner
                .seal_with_check(
                    agent,
                    summary.to_string(),
                    spec_id,
                    task_status,
                    verification,
                    allow_empty,
                    tracked_head,
                )
                .map_err(writ_err)?;
            self.inner.clear_context_head();
            let result = build_seal_result(&self.inner, seal, warning);
            to_pydict(py, &result)
        } else {
            let seal = self
                .inner
                .seal(
                    agent,
                    summary.to_string(),
                    spec_id,
                    task_status,
                    verification,
                    allow_empty,
                )
                .map_err(writ_err)?;
            let result = build_seal_result(&self.inner, seal, None);
            to_pydict(py, &result)
        }
    }

    /// Seal with optimistic conflict detection.
    ///
    /// Returns a dict with `seal` and optional `conflict_warning`.
    #[pyo3(signature = (summary, agent_id="human", agent_type="human", spec_id=None, status="complete", tests_passed=None, tests_failed=None, linted=false, allow_empty=false, expected_head=None))]
    fn seal_with_check(
        &self,
        py: Python,
        summary: &str,
        agent_id: &str,
        agent_type: &str,
        spec_id: Option<String>,
        status: &str,
        tests_passed: Option<u32>,
        tests_failed: Option<u32>,
        linted: bool,
        allow_empty: bool,
        expected_head: Option<String>,
    ) -> PyResult<PyObject> {
        let agent = AgentIdentity {
            id: agent_id.to_string(),
            agent_type: parse_agent_type(agent_type)?,
        };
        let task_status = parse_task_status(status)?;
        let verification = Verification {
            tests_passed,
            tests_failed,
            linted,
        };

        let (seal, warning) = self
            .inner
            .seal_with_check(
                agent,
                summary.to_string(),
                spec_id,
                task_status,
                verification,
                allow_empty,
                expected_head,
            )
            .map_err(writ_err)?;

        let result = build_seal_result(&self.inner, seal, warning);
        to_pydict(py, &result)
    }

    /// Get seal history (newest first).
    ///
    /// `format` controls the return type: "dict" (default), "json",
    /// "json-compact", or "toon".
    #[pyo3(signature = (limit=None, format="dict"))]
    fn log(&self, py: Python, limit: Option<usize>, format: &str) -> PyResult<PyObject> {
        let mut seals = self.inner.log().map_err(writ_err)?;
        if let Some(n) = limit {
            seals.truncate(n);
        }
        format_seals(py, &seals, format)
    }

    /// Get the seal chain for a specific spec, walking from its tip.
    ///
    /// `format` controls the return type: "dict" (default), "json",
    /// "json-compact", or "toon".
    #[pyo3(signature = (spec_id, limit=None, format="dict"))]
    fn spec_log(
        &self,
        py: Python,
        spec_id: &str,
        limit: Option<usize>,
        format: &str,
    ) -> PyResult<PyObject> {
        let mut seals = self.inner.spec_log(spec_id).map_err(writ_err)?;
        if let Some(n) = limit {
            seals.truncate(n);
        }
        format_seals(py, &seals, format)
    }

    /// Unified log across ALL heads (global + spec branches), deduped, newest-first.
    /// Shows seals from diverged branches that `log()` would miss.
    ///
    /// `format` controls the return type: "dict" (default), "json",
    /// "json-compact", or "toon".
    #[pyo3(signature = (limit=None, format="dict"))]
    fn log_all(&self, py: Python, limit: Option<usize>, format: &str) -> PyResult<PyObject> {
        let mut seals = self.inner.log_all().map_err(writ_err)?;
        if let Some(n) = limit {
            seals.truncate(n);
        }
        format_seals(py, &seals, format)
    }

    /// Get diverged branch information for multi-agent convergence.
    fn diverged_branches(&self, py: Python) -> PyResult<PyObject> {
        let diverged = self.inner.diverged_branches().map_err(writ_err)?;
        to_pydict(py, &diverged)
    }

    /// Get the tip seal ID for a specific spec.
    fn spec_head(&self, spec_id: &str) -> PyResult<Option<String>> {
        self.inner.spec_head(spec_id).map_err(writ_err)
    }

    /// Get high-level project status: agent activity, spec progress,
    /// commit readiness. Complements `state()` (low-level plumbing)
    /// with fleet-aware, progress-oriented porcelain.
    ///
    /// `format` controls the return type: "dict" (default), "json",
    /// "json-compact", or "toon".
    #[pyo3(signature = (format="dict"))]
    fn status(&self, py: Python, format: &str) -> PyResult<PyObject> {
        let status = self.inner.status().map_err(writ_err)?;
        format_status(py, &status, format)
    }

    /// Diff working tree against HEAD with optional filtering.
    ///
    /// Filtering parameters (all optional):
    /// - `spec`: Filter to files changed by a specific spec ID.
    /// - `agent`: Filter to files changed by a specific agent ID.
    /// - `completed`: Only show changes from completed specs.
    /// - `include_all`: When combined with `completed`, also include in-progress specs.
    /// - `file`: Filter to a single file path.
    ///
    /// `format` controls the return type: "dict" (default), "json",
    /// "json-compact", or "toon".
    #[pyo3(signature = (spec=None, agent=None, completed=false, include_all=false, file=None, format="dict"))]
    fn diff(
        &self,
        py: Python,
        spec: Option<&str>,
        agent: Option<&str>,
        completed: bool,
        include_all: bool,
        file: Option<&str>,
        format: &str,
    ) -> PyResult<PyObject> {
        let mut diff = self.inner.diff().map_err(writ_err)?;

        let has_filter = spec.is_some() || agent.is_some() || completed || file.is_some();
        if has_filter {
            let allowed = self.collect_filtered_paths(spec, agent, completed, include_all, file)?;
            diff.files.retain(|f| allowed.contains(&f.path));
            diff.files_changed = diff.files.len();
            diff.total_additions = diff.files.iter().map(|f| f.additions).sum();
            diff.total_deletions = diff.files.iter().map(|f| f.deletions).sum();
        }

        format_diff(py, &diff, format)
    }

    /// Diff between two seals (supports short ID prefixes).
    ///
    /// `format` controls the return type: "dict" (default), "json",
    /// "json-compact", or "toon".
    #[pyo3(signature = (from_id, to_id, format="dict"))]
    fn diff_seals(
        &self,
        py: Python,
        from_id: &str,
        to_id: &str,
        format: &str,
    ) -> PyResult<PyObject> {
        let diff = self.inner.diff_seals(from_id, to_id).map_err(writ_err)?;
        format_diff(py, &diff, format)
    }

    /// Diff a single seal vs its parent (or vs empty for first seal).
    ///
    /// `format` controls the return type: "dict" (default), "json",
    /// "json-compact", or "toon".
    #[pyo3(signature = (seal_id, format="dict"))]
    fn diff_seal(&self, py: Python, seal_id: &str, format: &str) -> PyResult<PyObject> {
        let diff = self.inner.diff_seal(seal_id).map_err(writ_err)?;
        format_diff(py, &diff, format)
    }

    /// Get structured context for LLM consumption.
    ///
    /// Optional filters narrow the seal history before `seal_limit` is applied:
    /// - `status`: "in-progress", "complete", or "blocked"
    /// - `agent`: agent ID string (filters seal history)
    /// - `for_agent`: agent ID string (scopes entire context to agent's world)
    ///
    /// `format` controls the return type:
    /// - `"dict"` (default): returns a parsed Python dict
    /// - `"json"`: returns a pretty-printed JSON string
    /// - `"json-compact"`: returns a minified JSON string
    /// - `"toon"`: returns a TOON string (~40% fewer tokens than JSON)
    #[pyo3(signature = (spec=None, seal_limit=10, status=None, agent=None, for_agent=None, format="dict"))]
    fn context(
        &self,
        py: Python,
        spec: Option<String>,
        seal_limit: usize,
        status: Option<String>,
        agent: Option<String>,
        for_agent: Option<String>,
        format: &str,
    ) -> PyResult<PyObject> {
        let scope = if let Some(id) = spec {
            ContextScope::Spec(id)
        } else if let Some(id) = for_agent {
            ContextScope::Agent(id)
        } else {
            ContextScope::Full
        };
        let filter_status = match status.as_deref() {
            Some("in-progress") => Some(TaskStatus::InProgress),
            Some("complete") => Some(TaskStatus::Complete),
            Some("blocked") => Some(TaskStatus::Blocked),
            Some(other) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown status filter: '{other}' (use in-progress, complete, or blocked)"
                )));
            }
            None => None,
        };
        let filter = ContextFilter {
            status: filter_status,
            agent,
        };
        let ctx = self
            .inner
            .context(scope, seal_limit, &filter)
            .map_err(writ_err)?;

        match format {
            "dict" => to_pydict(py, &ctx),
            "json" | "json-compact" | "toon" => {
                let formatter = writ_core::format::formatter_for(format)
                    .ok_or_else(|| WritError::new_err(format!("unknown format: '{format}'")))?;
                let output = formatter
                    .format_context(&ctx)
                    .map_err(|e| WritError::new_err(e.to_string()))?;
                Ok(pyo3::types::PyString::new(py, &output).into_any().unbind())
            }
            other => Err(WritError::new_err(format!(
                "unknown format: '{other}' (expected 'dict', 'json', 'json-compact', or 'toon')"
            ))),
        }
    }

    /// Load a seal by full or short ID.
    fn get_seal(&self, py: Python, seal_id: &str) -> PyResult<PyObject> {
        let seal = self.inner.get_seal(seal_id).map_err(writ_err)?;
        to_pydict(py, &seal)
    }

    /// Restore working directory to a specific seal's state.
    fn restore(&self, py: Python, seal_id: &str) -> PyResult<PyObject> {
        let result = self.inner.restore(seal_id).map_err(writ_err)?;
        to_pydict(py, &result)
    }

    /// Register a new spec. Returns the created spec as a dict.
    #[pyo3(signature = (id, title, description="", acceptance_criteria=None, design_notes=None, tech_stack=None))]
    fn add_spec(
        &self,
        py: Python,
        id: &str,
        title: &str,
        description: &str,
        acceptance_criteria: Option<Vec<String>>,
        design_notes: Option<Vec<String>>,
        tech_stack: Option<Vec<String>>,
    ) -> PyResult<PyObject> {
        let mut spec = Spec::new(id.to_string(), title.to_string(), description.to_string());
        if let Some(ac) = acceptance_criteria {
            spec.acceptance_criteria = ac;
        }
        if let Some(dn) = design_notes {
            spec.design_notes = dn;
        }
        if let Some(ts) = tech_stack {
            spec.tech_stack = ts;
        }
        self.inner.add_spec(&spec).map_err(writ_err)?;
        to_pydict(py, &spec)
    }

    /// Load a spec by ID.
    fn get_spec(&self, py: Python, id: &str) -> PyResult<PyObject> {
        let spec = self.inner.load_spec(id).map_err(writ_err)?;
        to_pydict(py, &spec)
    }

    /// Update a spec's mutable fields.
    #[pyo3(signature = (id, status=None, depends_on=None, file_scope=None, acceptance_criteria=None, design_notes=None, tech_stack=None))]
    fn update_spec(
        &self,
        py: Python,
        id: &str,
        status: Option<&str>,
        depends_on: Option<Vec<String>>,
        file_scope: Option<Vec<String>>,
        acceptance_criteria: Option<Vec<String>>,
        design_notes: Option<Vec<String>>,
        tech_stack: Option<Vec<String>>,
    ) -> PyResult<PyObject> {
        let parsed_status = match status {
            Some(s) => Some(parse_spec_status(s)?),
            None => None,
        };

        let update = SpecUpdate {
            status: parsed_status,
            depends_on,
            file_scope,
            acceptance_criteria,
            design_notes,
            tech_stack,
        };

        let spec = self.inner.update_spec(id, update).map_err(writ_err)?;
        to_pydict(py, &spec)
    }

    /// List all specs.
    ///
    /// `format` controls the return type: "dict" (default), "json",
    /// "json-compact", or "toon".
    #[pyo3(signature = (format="dict"))]
    fn list_specs(&self, py: Python, format: &str) -> PyResult<PyObject> {
        let specs = self.inner.list_specs().map_err(writ_err)?;
        format_specs(py, &specs, format)
    }

    /// Analyze convergence between two specs (three-way merge).
    ///
    /// Returns a ConvergenceReport dict with auto_merged, conflicts,
    /// left_only, right_only, and is_clean fields.
    fn converge(&self, py: Python, left_spec: &str, right_spec: &str) -> PyResult<PyObject> {
        let report = self
            .inner
            .converge(left_spec, right_spec)
            .map_err(writ_err)?;
        to_pydict(py, &report)
    }

    /// Apply a convergence result to the working directory.
    ///
    /// Writes merged files and resolved conflicts to disk.
    /// Does NOT create a seal — call `seal()` after to capture the result.
    ///
    /// `report` should be the dict returned by `converge()`.
    /// `resolutions` is a list of dicts with `path` and `content` keys
    /// (only needed if the report has conflicts).
    #[pyo3(signature = (report, resolutions=None))]
    fn apply_convergence(
        &self,
        py: Python,
        report: PyObject,
        resolutions: Option<PyObject>,
    ) -> PyResult<()> {
        let report: writ_core::convergence::ConvergenceReport =
            pythonize::depythonize(&report.bind(py))
                .map_err(|e| WritError::new_err(e.to_string()))?;

        let resolutions: Vec<writ_core::convergence::FileResolution> = match resolutions {
            Some(r) => pythonize::depythonize(&r.bind(py))
                .map_err(|e| WritError::new_err(e.to_string()))?,
            None => Vec::new(),
        };

        self.inner
            .apply_convergence(&report, &resolutions)
            .map_err(writ_err)?;

        Ok(())
    }

    /// Converge ALL diverged branches in sequence.
    ///
    /// Returns a ConvergeAllReport dict with per-step merge results,
    /// conflict resolutions, and warnings about potential content loss.
    ///
    /// `strategy` controls the fallback for irreconcilable conflicts:
    /// "escalate" (default) records full context for review; "manual" leaves
    /// unresolved; "most-recent" (deprecated) picks the most recently sealed
    /// version; "orchestrator" returns structured data.
    /// Deterministic patterns always run regardless of strategy.
    ///
    /// When `apply` is True, merged files are written to the working directory.
    #[pyo3(signature = (strategy="escalate", apply=false))]
    fn converge_all(&self, py: Python, strategy: &str, apply: bool) -> PyResult<PyObject> {
        let strat = match strategy {
            "escalate" => writ_core::convergence::ConvergeStrategy::Escalate,
            "most-recent" => {
                eprintln!(
                    "writ warning: 'most-recent' strategy is deprecated; use 'escalate' instead"
                );
                #[allow(deprecated)]
                writ_core::convergence::ConvergeStrategy::MostRecent
            }
            "orchestrator" => writ_core::convergence::ConvergeStrategy::Orchestrator,
            "manual" => writ_core::convergence::ConvergeStrategy::Manual,
            _ => writ_core::convergence::ConvergeStrategy::Escalate,
        };
        let report = self.inner.converge_all(strat, apply).map_err(writ_err)?;
        to_pydict(py, &report)
    }

    /// Import git state as a writ baseline seal.
    #[pyo3(signature = (git_ref="HEAD", agent_id="bridge", agent_type="agent"))]
    fn bridge_import(
        &self,
        py: Python,
        git_ref: &str,
        agent_id: &str,
        agent_type: &str,
    ) -> PyResult<PyObject> {
        let agent = AgentIdentity {
            id: agent_id.to_string(),
            agent_type: parse_agent_type(agent_type)?,
        };
        let result = self
            .inner
            .bridge_import(Some(git_ref), agent)
            .map_err(writ_err)?;
        to_pydict(py, &result)
    }

    /// Export writ seals as git commits on a branch.
    #[pyo3(signature = (branch="writ/export"))]
    fn bridge_export(&self, py: Python, branch: &str) -> PyResult<PyObject> {
        let result = self.inner.bridge_export(Some(branch)).map_err(writ_err)?;
        to_pydict(py, &result)
    }

    /// Get bridge sync status.
    fn bridge_status(&self, py: Python) -> PyResult<PyObject> {
        let status = self.inner.bridge_status().map_err(writ_err)?;
        to_pydict(py, &status)
    }

    /// Human-readable summary of all work done in this writ session.
    fn summary(&self, py: Python) -> PyResult<PyObject> {
        let summary = self.inner.summary().map_err(writ_err)?;
        to_pydict(py, &summary)
    }

    /// Initialize a bare remote directory.
    #[staticmethod]
    fn remote_init(path: &str) -> PyResult<()> {
        let p = PathBuf::from(path);
        writ_core::Repository::remote_init(&p).map_err(writ_err)?;
        Ok(())
    }

    /// Add a named remote to this repository's config.
    fn remote_add(&self, name: &str, path: &str) -> PyResult<()> {
        self.inner.remote_add(name, path).map_err(writ_err)?;
        Ok(())
    }

    /// Remove a named remote.
    fn remote_remove(&self, name: &str) -> PyResult<()> {
        self.inner.remote_remove(name).map_err(writ_err)?;
        Ok(())
    }

    /// List all configured remotes as a dict.
    fn remote_list(&self, py: Python) -> PyResult<PyObject> {
        let remotes = self.inner.remote_list().map_err(writ_err)?;
        to_pydict(py, &remotes)
    }

    /// Push local state to a named remote.
    #[pyo3(signature = (remote="origin"))]
    fn push(&self, py: Python, remote: &str) -> PyResult<PyObject> {
        let result = self.inner.push(remote).map_err(writ_err)?;
        to_pydict(py, &result)
    }

    /// Pull remote state into local.
    #[pyo3(signature = (remote="origin"))]
    fn pull(&self, py: Python, remote: &str) -> PyResult<PyObject> {
        let result = self.inner.pull(remote).map_err(writ_err)?;
        to_pydict(py, &result)
    }

    /// Get sync status with a remote.
    #[pyo3(signature = (remote="origin"))]
    fn remote_status(&self, py: Python, remote: &str) -> PyResult<PyObject> {
        let status = self.inner.remote_status(remote).map_err(writ_err)?;
        to_pydict(py, &status)
    }

    /// Verify the cryptographic integrity of the seal chain from HEAD.
    ///
    /// Returns a dict with: total_seals, verified, unsecured, failures, valid.
    /// If `use_convergence_key` is True, uses the repo's convergence verifying
    /// key for signature verification. Otherwise signatures are not checked.
    #[pyo3(signature = (use_convergence_key=false))]
    fn verify_chain(&self, py: Python, use_convergence_key: bool) -> PyResult<PyObject> {
        let vk = if use_convergence_key {
            self.inner.convergence_verifying_key()
        } else {
            None
        };
        let result = self.inner.verify_chain(vk.as_ref()).map_err(writ_err)?;
        to_pydict(py, &result)
    }

    /// Verify a single seal's cryptographic integrity.
    ///
    /// Returns a dict with: seal_id, content_hash_valid, chain_hash_valid,
    /// signature_present, signature_valid, error.
    #[pyo3(signature = (seal_id, use_convergence_key=false))]
    fn verify_seal(
        &self,
        py: Python,
        seal_id: &str,
        use_convergence_key: bool,
    ) -> PyResult<PyObject> {
        let seal = self.inner.get_seal(seal_id).map_err(writ_err)?;
        let vk = if use_convergence_key {
            self.inner.convergence_verifying_key()
        } else {
            None
        };
        let result = self.inner.verify_seal(&seal, vk.as_ref());
        to_pydict(py, &result)
    }

    // -------------------------------------------------------------------
    // GC & lifecycle
    // -------------------------------------------------------------------

    /// Get a storage report for this repository.
    ///
    /// Returns a dict with: total_bytes, seal_bytes, working_state_bytes,
    /// security_event_bytes, key_bytes, agent_bytes, gc_bytes, other_bytes,
    /// budget_bytes.
    fn storage_report(&self, py: Python) -> PyResult<PyObject> {
        let report = self.inner.storage_report().map_err(writ_err)?;
        to_pydict(py, &report)
    }

    /// Get GC status: storage report, spec lifecycle counts, stale warnings.
    ///
    /// Returns a dict with storage breakdown, per-state spec counts, and
    /// stale spec candidates.
    fn gc_status(&self, py: Python) -> PyResult<PyObject> {
        let writ_dir = self.inner.writ_dir();
        let config = writ_core::gc::GcConfig::load(writ_dir).map_err(writ_err)?;
        let specs = self.inner.list_specs().map_err(writ_err)?;
        let storage =
            writ_core::gc::StorageReport::scan(writ_dir, config.budget_bytes).map_err(writ_err)?;
        let stale = self.inner.scan_stale_specs(&config).map_err(writ_err)?;

        let mut active = 0usize;
        let mut stale_count = 0usize;
        let mut completed = 0usize;
        let mut cancelled = 0usize;
        let mut archived = 0usize;

        for spec in &specs {
            match spec.lifecycle_state {
                writ_core::spec::LifecycleState::Active => active += 1,
                writ_core::spec::LifecycleState::Stale => stale_count += 1,
                writ_core::spec::LifecycleState::Completed => completed += 1,
                writ_core::spec::LifecycleState::Cancelled => cancelled += 1,
                writ_core::spec::LifecycleState::Archived => archived += 1,
            }
        }

        // Orphan analysis
        let all_seals = writ_core::gc::load_all_seals(writ_dir).map_err(writ_err)?;
        let orphans =
            writ_core::gc::find_orphaned_objects(writ_dir, &all_seals).map_err(writ_err)?;
        let orphan_bytes: u64 = orphans.iter().map(|o| o.size_bytes).sum();

        let result = serde_json::json!({
            "storage": storage,
            "usage_pct": storage.usage_pct(),
            "specs": {
                "total": specs.len(),
                "active": active,
                "stale": stale_count,
                "completed": completed,
                "cancelled": cancelled,
                "archived": archived,
            },
            "stale_candidates": stale.iter().map(|(id, secs)| {
                serde_json::json!({"spec_id": id, "inactive_seconds": secs})
            }).collect::<Vec<_>>(),
            "orphaned_objects": orphans.len(),
            "orphaned_bytes": orphan_bytes,
            "mode": config.mode,
            "budget_bytes": config.budget_bytes,
        });
        to_pydict(py, &result)
    }

    /// Generate a GC plan without executing it (dry run).
    ///
    /// Returns a dict with: generated_at, storage, actions, summary.
    fn gc_dry_run(&self, py: Python) -> PyResult<PyObject> {
        let writ_dir = self.inner.writ_dir();
        let config = writ_core::gc::GcConfig::load(writ_dir).map_err(writ_err)?;
        let specs = self.inner.list_specs().map_err(writ_err)?;
        let logger = writ_core::security::SecurityEventLogger::new(writ_dir);
        let events = logger.read_events(None).map_err(writ_err)?;
        let plan = writ_core::gc::GcPlan::generate(writ_dir, &config, &specs, &events)
            .map_err(writ_err)?;
        to_pydict(py, &plan)
    }

    /// Run garbage collection (generate plan and execute).
    ///
    /// Returns a dict with: audit record, specs_cleaned, events_cleaned,
    /// transitions_applied.
    fn gc(&self, py: Python) -> PyResult<PyObject> {
        let writ_dir = self.inner.writ_dir();
        let config = writ_core::gc::GcConfig::load(writ_dir).map_err(writ_err)?;
        let specs = self.inner.list_specs().map_err(writ_err)?;
        let logger = writ_core::security::SecurityEventLogger::new(writ_dir);
        let events = logger.read_events(None).map_err(writ_err)?;
        let plan = writ_core::gc::GcPlan::generate(writ_dir, &config, &specs, &events)
            .map_err(writ_err)?;
        let result = writ_core::gc::execute_plan(writ_dir, &plan, &specs).map_err(writ_err)?;

        // If events were marked for cleaning, actually clean the events file.
        if result.events_cleaned > 0 {
            logger
                .clean_events(&config.security_events)
                .map_err(writ_err)?;
        }

        let output = serde_json::json!({
            "audit": result.audit,
            "specs_cleaned": result.specs_cleaned,
            "events_cleaned": result.events_cleaned,
            "transitions_applied": result.transitions_applied.iter().map(|(id, from, to)| {
                serde_json::json!({"spec_id": id, "from": from, "to": to})
            }).collect::<Vec<_>>(),
            "objects_pruned": result.objects_pruned,
            "bytes_freed": result.bytes_freed,
            "objects_recompressed": result.objects_recompressed,
            "recompression_savings": result.recompression_savings,
        });
        to_pydict(py, &output)
    }

    /// Cancel a spec (transition lifecycle to Cancelled).
    ///
    /// Allowed from Active or Stale states.
    fn cancel_spec(&self, spec_id: &str) -> PyResult<()> {
        self.inner.cancel_spec(spec_id).map_err(writ_err)?;
        Ok(())
    }

    /// Complete a spec's lifecycle (transition to Completed).
    ///
    /// Requires the spec's user-facing status to already be 'complete'.
    fn complete_spec(&self, spec_id: &str) -> PyResult<()> {
        self.inner.complete_spec(spec_id).map_err(writ_err)?;
        Ok(())
    }

    /// Commit completed spec work to git (programmatic `writ finish`).
    ///
    /// Parameters:
    /// - `strategy`: "single" (default) or "per-spec"
    /// - `message`: Optional commit message override. If None, auto-generates from spec summaries.
    /// - `dry_run`: If True, returns what would be committed without actually committing.
    /// - `specs`: Optional list of spec IDs to finish. If None, finishes all committable specs.
    ///
    /// Returns a dict with `commits` (list of {hash, message, specs}), `strategy`, `dry_run`.
    #[pyo3(signature = (strategy="single", message=None, dry_run=false, specs=None))]
    fn finish(
        &self,
        py: Python,
        strategy: &str,
        message: Option<String>,
        dry_run: bool,
        specs: Option<Vec<String>>,
    ) -> PyResult<PyObject> {
        use serde::Serialize;
        use writ_core::git_ops::{Git2Ops, GitOps};

        #[derive(Serialize)]
        struct FinishCommit {
            hash: String,
            message: String,
            specs: Vec<String>,
        }

        #[derive(Serialize)]
        struct FinishResult {
            commits: Vec<FinishCommit>,
            strategy: String,
            dry_run: bool,
            specs_finished: usize,
        }

        // Validate strategy
        if strategy != "single" && strategy != "per-spec" {
            return Err(WritError::new_err(format!(
                "unknown strategy: '{}' (expected 'single' or 'per-spec')",
                strategy
            )));
        }

        let all_specs = self.inner.list_specs().map_err(writ_err)?;
        let committable: Vec<_> = all_specs
            .iter()
            .filter(|s| {
                if !s.is_committable() {
                    return false;
                }
                match &specs {
                    Some(ids) => ids.iter().any(|id| id == &s.id),
                    None => true,
                }
            })
            .collect();

        if committable.is_empty() {
            let result = FinishResult {
                commits: Vec::new(),
                strategy: strategy.to_string(),
                dry_run,
                specs_finished: 0,
            };
            return to_pydict(py, &result);
        }

        if dry_run {
            // Return what would be committed without doing it
            let spec_ids: Vec<String> = committable.iter().map(|s| s.id.clone()).collect();
            let msg = message.unwrap_or_else(|| {
                committable
                    .iter()
                    .map(|s| s.completion_summary.as_deref().unwrap_or(&s.title))
                    .collect::<Vec<_>>()
                    .join("; ")
            });
            let commits = match strategy {
                "per-spec" => committable
                    .iter()
                    .map(|s| FinishCommit {
                        hash: "(dry-run)".to_string(),
                        message: format!(
                            "{}: {}",
                            s.id,
                            s.completion_summary.as_deref().unwrap_or(&s.title)
                        ),
                        specs: vec![s.id.clone()],
                    })
                    .collect(),
                _ => vec![FinishCommit {
                    hash: "(dry-run)".to_string(),
                    message: msg,
                    specs: spec_ids,
                }],
            };
            let result = FinishResult {
                specs_finished: committable.len(),
                commits,
                strategy: strategy.to_string(),
                dry_run: true,
            };
            return to_pydict(py, &result);
        }

        // Open git repo
        let root = self.inner.root();
        let git = Git2Ops::open(root).map_err(|e| WritError::new_err(e.to_string()))?;

        let mut commits = Vec::new();

        match strategy {
            "per-spec" => {
                let mut sorted: Vec<_> = committable.clone();
                sorted.sort_by_key(|s| s.completed_at);

                for s in &sorted {
                    // Stage files in this spec's scope if available, otherwise stage all
                    if !s.file_scope.is_empty() {
                        let paths: Vec<&str> = s.file_scope.iter().map(|p| p.as_str()).collect();
                        git.stage_files(&paths)
                            .map_err(|e| WritError::new_err(e.to_string()))?;
                    } else {
                        git.stage_all()
                            .map_err(|e| WritError::new_err(e.to_string()))?;
                    }

                    if !git
                        .has_staged_changes()
                        .map_err(|e| WritError::new_err(e.to_string()))?
                    {
                        continue;
                    }

                    let msg = format!(
                        "{}: {}",
                        s.id,
                        s.completion_summary.as_deref().unwrap_or(&s.title)
                    );
                    let hash = git
                        .commit(&msg)
                        .map_err(|e| WritError::new_err(e.to_string()))?;
                    let _ = self.inner.mark_spec_committed(&s.id, &hash);
                    commits.push(FinishCommit {
                        hash,
                        message: msg,
                        specs: vec![s.id.clone()],
                    });
                }
            }
            _ => {
                // Single commit strategy
                git.stage_all()
                    .map_err(|e| WritError::new_err(e.to_string()))?;

                if !git
                    .has_staged_changes()
                    .map_err(|e| WritError::new_err(e.to_string()))?
                {
                    let result = FinishResult {
                        commits: Vec::new(),
                        strategy: strategy.to_string(),
                        dry_run: false,
                        specs_finished: 0,
                    };
                    return to_pydict(py, &result);
                }

                let spec_ids: Vec<String> = committable.iter().map(|s| s.id.clone()).collect();
                let msg = message.unwrap_or_else(|| {
                    let summary = self.inner.summary().ok();
                    summary
                        .map(|s| s.headline)
                        .unwrap_or_else(|| "writ: commit completed specs".to_string())
                });

                let hash = git
                    .commit(&msg)
                    .map_err(|e| WritError::new_err(e.to_string()))?;
                for s in &committable {
                    let _ = self.inner.mark_spec_committed(&s.id, &hash);
                }
                commits.push(FinishCommit {
                    hash,
                    message: msg,
                    specs: spec_ids,
                });
            }
        }

        let specs_finished = commits.iter().map(|c| c.specs.len()).sum();
        let result = FinishResult {
            commits,
            strategy: strategy.to_string(),
            dry_run: false,
            specs_finished,
        };
        to_pydict(py, &result)
    }

    /// Mark a spec as done: sets status to Complete, stores the optional
    /// completion summary, and records the completion timestamp.
    ///
    /// This is the Python binding for `writ spec done <id>`.
    /// Returns the updated spec as a dict.
    #[pyo3(signature = (spec_id, summary=None))]
    fn spec_done(&self, py: Python, spec_id: &str, summary: Option<String>) -> PyResult<PyObject> {
        let spec = self
            .inner
            .mark_spec_done(spec_id, summary)
            .map_err(writ_err)?;
        to_pydict(py, &spec)
    }

    /// Reopen a completed spec, returning it to active/in-progress state.
    ///
    /// Only uncommitted completed specs can be reopened. The seal chain is
    /// preserved — a new or existing agent can pick up the spec and continue.
    fn reopen_spec(&self, spec_id: &str) -> PyResult<()> {
        self.inner.reopen_spec(spec_id).map_err(writ_err)?;
        Ok(())
    }

    // -- Propose mode (W.31) --

    /// Create a commit proposal for review (propose mode).
    ///
    /// Parameters:
    /// - `spec_ids`: List of spec IDs to include in the proposal.
    /// - `message`: Proposed commit message.
    /// - `proposed_by`: Who created this proposal (agent ID or orchestrator name).
    /// - `strategy`: Commit strategy ("single" or "per-spec").
    ///
    /// Any pending proposals with overlapping specs are automatically superseded.
    /// Returns the proposal as a dict.
    #[pyo3(signature = (spec_ids, message, proposed_by="cli", strategy="single"))]
    fn propose(
        &self,
        py: Python,
        spec_ids: Vec<String>,
        message: String,
        proposed_by: &str,
        strategy: &str,
    ) -> PyResult<PyObject> {
        let proposal = self
            .inner
            .create_proposal(
                spec_ids,
                message,
                proposed_by.to_string(),
                strategy.to_string(),
            )
            .map_err(writ_err)?;
        to_pydict(py, &proposal)
    }

    /// List all proposals, sorted by creation time (newest first).
    /// Returns a list of proposal dicts.
    fn list_proposals(&self, py: Python) -> PyResult<PyObject> {
        let proposals = self.inner.list_proposals().map_err(writ_err)?;
        to_pydict(py, &proposals)
    }

    /// Accept a pending proposal: marks it accepted.
    /// The actual git commit should be done via `finish()` or externally.
    /// Returns the updated proposal as a dict.
    fn accept_proposal(&self, py: Python, proposal_id: &str) -> PyResult<PyObject> {
        let proposal = self.inner.accept_proposal(proposal_id).map_err(writ_err)?;
        to_pydict(py, &proposal)
    }

    /// Reject a pending proposal. Specs remain completed for future proposals.
    /// Returns the updated proposal as a dict.
    fn reject_proposal(&self, py: Python, proposal_id: &str) -> PyResult<PyObject> {
        let proposal = self.inner.reject_proposal(proposal_id).map_err(writ_err)?;
        to_pydict(py, &proposal)
    }

    // -----------------------------------------------------------------------
    // Upgrade & migration bindings (UPG.12)
    // -----------------------------------------------------------------------

    /// Run health checks on the repository. Returns a dict with:
    ///   checks: list of {name, status, message}
    ///   passed: int
    ///   failed: int
    ///   warnings: int
    ///   is_healthy: bool
    fn doctor(&self, py: Python) -> PyResult<PyObject> {
        let writ_dir = self.inner.writ_dir();
        let report = writ_core::migrate::DoctorReport::run(writ_dir);
        let dict = pyo3::types::PyDict::new(py);
        let checks: Vec<_> = report
            .checks
            .iter()
            .map(|c| {
                let d = pyo3::types::PyDict::new(py);
                d.set_item("name", &c.name).unwrap();
                d.set_item(
                    "status",
                    match c.status {
                        writ_core::migrate::CheckStatus::Pass => "pass",
                        writ_core::migrate::CheckStatus::Fail => "fail",
                        writ_core::migrate::CheckStatus::Warning => "warning",
                    },
                )
                .unwrap();
                d.set_item("message", &c.message).unwrap();
                d.to_object(py)
            })
            .collect();
        dict.set_item("checks", checks)?;
        dict.set_item("passed", report.passed)?;
        dict.set_item("failed", report.failed)?;
        dict.set_item("warnings", report.warnings)?;
        dict.set_item("is_healthy", report.is_healthy())?;
        Ok(dict.to_object(py))
    }

    /// Return version metadata for this repository. Returns a dict with:
    ///   schema_version: int
    ///   created_by: str
    ///   last_opened_by: str
    ///   created_at: str or None
    ///   last_opened_at: str or None
    fn version_info(&self, py: Python) -> PyResult<PyObject> {
        let writ_dir = self.inner.writ_dir();
        let version = writ_core::migrate::RepoVersion::load(writ_dir).map_err(writ_err)?;
        match version {
            Some(v) => to_pydict(py, &v),
            None => {
                // Legacy repo — return minimal info
                let dict = pyo3::types::PyDict::new(py);
                dict.set_item("schema_version", 0)?;
                dict.set_item("created_by", pyo3::types::PyNone::get(py))?;
                dict.set_item("last_opened_by", pyo3::types::PyNone::get(py))?;
                dict.set_item("created_at", pyo3::types::PyNone::get(py))?;
                dict.set_item("last_opened_at", pyo3::types::PyNone::get(py))?;
                Ok(dict.to_object(py))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers for PyRepository
// ---------------------------------------------------------------------------

impl PyRepository {
    /// Collect file paths matching filter criteria (mirrors CLI's collect_filtered_paths).
    fn collect_filtered_paths(
        &self,
        spec: Option<&str>,
        agent: Option<&str>,
        completed: bool,
        include_all: bool,
        file: Option<&str>,
    ) -> PyResult<std::collections::HashSet<String>> {
        use std::collections::HashSet;

        // Single file filter — just that path.
        if let Some(path) = file {
            let mut set = HashSet::new();
            set.insert(path.to_string());
            return Ok(set);
        }

        let mut paths = HashSet::new();

        if let Some(spec_id) = spec {
            if let Ok(seals) = self.inner.spec_log(spec_id) {
                for seal in &seals {
                    for change in &seal.changes {
                        paths.insert(change.path.clone());
                    }
                }
            }
            return Ok(paths);
        }

        if agent.is_some() || completed {
            let seals = self.inner.log_all().map_err(writ_err)?;

            let completed_specs: HashSet<String> = if completed && !include_all {
                self.inner
                    .list_specs()
                    .map_err(writ_err)?
                    .iter()
                    .filter(|s| s.status == writ_core::spec::SpecStatus::Complete)
                    .map(|s| s.id.clone())
                    .collect()
            } else {
                HashSet::new()
            };

            for seal in &seals {
                if let Some(agent_name) = agent {
                    if seal.agent.id != agent_name {
                        continue;
                    }
                }
                if completed && !include_all {
                    match &seal.spec_id {
                        Some(sid) if completed_specs.contains(sid) => {}
                        _ => continue,
                    }
                }
                for change in &seal.changes {
                    paths.insert(change.path.clone());
                }
            }
        }

        Ok(paths)
    }
}

// ---------------------------------------------------------------------------
// Module registration
// ---------------------------------------------------------------------------

/// Detect agent frameworks in a project directory.
#[pyfunction]
#[pyo3(name = "detect_frameworks")]
fn py_detect_frameworks(py: Python, path: &str) -> PyResult<PyObject> {
    let p = PathBuf::from(path);
    let detections = writ_core::hooks::detect_frameworks(&p);
    to_pydict(py, &detections)
}

/// Install writ hooks for all detected agent frameworks.
#[pyfunction]
#[pyo3(name = "install_hooks")]
fn py_install_hooks(py: Python, path: &str) -> PyResult<PyObject> {
    let p = PathBuf::from(path);
    let results = writ_core::hooks::install_hooks(&p).map_err(writ_err)?;
    to_pydict(py, &results)
}

#[pymodule]
#[pyo3(name = "_native")]
fn writ_native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRepository>()?;
    m.add_class::<PyAgentType>()?;
    m.add_class::<PyTaskStatus>()?;
    m.add_class::<PySpecStatus>()?;
    m.add("WritError", m.py().get_type::<WritError>())?;
    m.add_function(wrap_pyfunction!(py_detect_frameworks, m)?)?;
    m.add_function(wrap_pyfunction!(py_install_hooks, m)?)?;
    Ok(())
}
