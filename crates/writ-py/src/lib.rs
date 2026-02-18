//! Python bindings for writ — AI-native version control.
//!
//! Exposes the full writ-core API to Python via PyO3.
//! Return types are Python dicts (via pythonize) for maximum
//! agent/LLM friendliness.

use std::path::PathBuf;

use pyo3::prelude::*;

use writ_core::context::ContextScope;
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

fn to_pydict<T: serde::Serialize>(py: Python, value: &T) -> PyResult<PyObject> {
    let obj = pythonize::pythonize(py, value)
        .map_err(|e| WritError::new_err(e.to_string()))?;
    Ok(obj.unbind())
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

    /// Get working directory state as a dict.
    fn state(&self, py: Python) -> PyResult<PyObject> {
        let state = self.inner.state().map_err(writ_err)?;
        to_pydict(py, &state)
    }

    /// Create a seal from current changes.
    ///
    /// If `paths` is provided, only seal matching files (selective seal).
    /// Otherwise seals all changes.
    #[pyo3(signature = (summary, agent_id="human", agent_type="human", spec_id=None, status="complete", paths=None, tests_passed=None, tests_failed=None, linted=false))]
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

        let seal = if let Some(ref p) = paths {
            self.inner
                .seal_paths(
                    agent,
                    summary.to_string(),
                    spec_id,
                    task_status,
                    verification,
                    p,
                )
                .map_err(writ_err)?
        } else {
            self.inner
                .seal(
                    agent,
                    summary.to_string(),
                    spec_id,
                    task_status,
                    verification,
                )
                .map_err(writ_err)?
        };

        to_pydict(py, &seal)
    }

    /// Get seal history (newest first).
    #[pyo3(signature = (limit=None))]
    fn log(&self, py: Python, limit: Option<usize>) -> PyResult<PyObject> {
        let mut seals = self.inner.log().map_err(writ_err)?;
        if let Some(n) = limit {
            seals.truncate(n);
        }
        to_pydict(py, &seals)
    }

    /// Diff working tree against HEAD.
    fn diff(&self, py: Python) -> PyResult<PyObject> {
        let diff = self.inner.diff().map_err(writ_err)?;
        to_pydict(py, &diff)
    }

    /// Diff between two seals (supports short ID prefixes).
    fn diff_seals(&self, py: Python, from_id: &str, to_id: &str) -> PyResult<PyObject> {
        let diff = self.inner.diff_seals(from_id, to_id).map_err(writ_err)?;
        to_pydict(py, &diff)
    }

    /// Diff a single seal vs its parent (or vs empty for first seal).
    fn diff_seal(&self, py: Python, seal_id: &str) -> PyResult<PyObject> {
        let diff = self.inner.diff_seal(seal_id).map_err(writ_err)?;
        to_pydict(py, &diff)
    }

    /// Get structured context for LLM consumption.
    #[pyo3(signature = (spec=None, seal_limit=10))]
    fn context(
        &self,
        py: Python,
        spec: Option<String>,
        seal_limit: usize,
    ) -> PyResult<PyObject> {
        let scope = match spec {
            Some(id) => ContextScope::Spec(id),
            None => ContextScope::Full,
        };
        let ctx = self.inner.context(scope, seal_limit).map_err(writ_err)?;
        to_pydict(py, &ctx)
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
    #[pyo3(signature = (id, title, description=""))]
    fn add_spec(&self, py: Python, id: &str, title: &str, description: &str) -> PyResult<PyObject> {
        let spec = Spec::new(
            id.to_string(),
            title.to_string(),
            description.to_string(),
        );
        self.inner.add_spec(&spec).map_err(writ_err)?;
        to_pydict(py, &spec)
    }

    /// Load a spec by ID.
    fn get_spec(&self, py: Python, id: &str) -> PyResult<PyObject> {
        let spec = self.inner.load_spec(id).map_err(writ_err)?;
        to_pydict(py, &spec)
    }

    /// Update a spec's mutable fields.
    #[pyo3(signature = (id, status=None, depends_on=None, file_scope=None))]
    fn update_spec(
        &self,
        py: Python,
        id: &str,
        status: Option<&str>,
        depends_on: Option<Vec<String>>,
        file_scope: Option<Vec<String>>,
    ) -> PyResult<PyObject> {
        let parsed_status = match status {
            Some(s) => Some(parse_spec_status(s)?),
            None => None,
        };

        let update = SpecUpdate {
            status: parsed_status,
            depends_on,
            file_scope,
        };

        let spec = self.inner.update_spec(id, update).map_err(writ_err)?;
        to_pydict(py, &spec)
    }

    /// List all specs.
    fn list_specs(&self, py: Python) -> PyResult<PyObject> {
        let specs = self.inner.list_specs().map_err(writ_err)?;
        to_pydict(py, &specs)
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
}

// ---------------------------------------------------------------------------
// Module registration
// ---------------------------------------------------------------------------

#[pymodule]
fn writ(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRepository>()?;
    m.add_class::<PyAgentType>()?;
    m.add_class::<PyTaskStatus>()?;
    m.add_class::<PySpecStatus>()?;
    m.add("WritError", m.py().get_type::<WritError>())?;
    Ok(())
}
