"""Project scaffold generators for test runs.

Each scaffold creates a realistic baseline project in a workspace directory.
These simulate the kind of codebase that real agents would work on.
"""

import os
from pathlib import Path
from typing import Callable


def get_scaffold(domain: str) -> Callable[[Path], None]:
    """Get the scaffold function for a domain."""
    scaffolds = {
        "taskapp": scaffold_taskapp,
        "Task management app": scaffold_taskapp,
    }
    fn = scaffolds.get(domain)
    if fn is None:
        raise ValueError(f"Unknown scaffold domain: {domain}. Available: {list(scaffolds.keys())}")
    return fn


def _write_file(workspace: Path, rel_path: str, content: str) -> None:
    """Write a file to the workspace, creating directories as needed."""
    full_path = workspace / rel_path
    full_path.parent.mkdir(parents=True, exist_ok=True)
    full_path.write_text(content)


def scaffold_taskapp(workspace: Path) -> None:
    """Create a task management app baseline.

    Structure:
        api/app.py          — Flask routes (/, /health)
        api/models.py       — Task, User models
        api/auth.py         — Basic auth helpers
        api/__init__.py     — Package init
        web/src/App.tsx     — React shell
        web/src/api.ts      — API client
        requirements.txt    — Python dependencies
        package.json        — JS dependencies
    """

    _write_file(workspace, "requirements.txt", """\
flask>=3.0
flask-cors
sqlalchemy>=2.0
pydantic>=2.0
pytest
""")

    _write_file(workspace, "package.json", """\
{
  "name": "taskapp-frontend",
  "dependencies": {
    "react": "^18.0.0",
    "react-dom": "^18.0.0",
    "react-router-dom": "^6.0.0",
    "axios": "^1.6.0"
  }
}
""")

    _write_file(workspace, "api/__init__.py", "")

    _write_file(workspace, "api/app.py", """\
from flask import Flask, jsonify
from flask_cors import CORS

app = Flask(__name__)
CORS(app)


@app.route("/")
def index():
    return jsonify({"service": "taskapp-api", "version": "1.0"})


@app.route("/health")
def health():
    return jsonify({"status": "ok"})


if __name__ == "__main__":
    app.run(debug=True)
""")

    _write_file(workspace, "api/models.py", """\
from pydantic import BaseModel
from datetime import datetime
from typing import Optional


class Task(BaseModel):
    id: int
    title: str
    description: str = ""
    completed: bool = False
    created_at: datetime = datetime.now()


class User(BaseModel):
    id: int
    username: str
    email: str
""")

    _write_file(workspace, "api/auth.py", """\
from functools import wraps


def require_auth(f):
    \"\"\"Decorator to require authentication.\"\"\"
    @wraps(f)
    def decorated(*args, **kwargs):
        # TODO: implement real auth check
        return f(*args, **kwargs)
    return decorated
""")

    _write_file(workspace, "web/src/App.tsx", """\
import React from 'react';
import { BrowserRouter, Route, Routes } from 'react-router-dom';

function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<div>TaskApp</div>} />
      </Routes>
    </BrowserRouter>
  );
}

export default App;
""")

    _write_file(workspace, "web/src/api.ts", """\
import axios from 'axios';

const API = axios.create({ baseURL: '/api' });

export async function fetchHealth() {
  const { data } = await API.get('/health');
  return data;
}
""")
