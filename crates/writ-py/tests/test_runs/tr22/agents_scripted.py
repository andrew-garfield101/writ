"""Scripted agent implementations for TR22.

Each function simulates an agent's work by making file changes and sealing.
Functions receive (workspace: Path, repo: Repository) and return a list of
changed file paths.

Naming convention: function name = agent name with dashes replaced by underscores.
  "backend-dev" -> backend_dev()
  "frontend-dev" -> frontend_dev()
  "rogue-agent" -> rogue_agent()
"""

from pathlib import Path
from typing import Any


def backend_dev(workspace: Path, repo: Any) -> list[str]:
    """Backend developer: adds CRUD routes, new models (Project, Comment).

    Modifies: api/app.py, api/models.py
    Touches the same files as baseline, simulating real agent overlap.
    """

    # Add Project and Comment models to models.py
    (workspace / "api" / "models.py").write_text("""\
from pydantic import BaseModel
from datetime import datetime
from typing import Optional


class Task(BaseModel):
    id: int
    title: str
    description: str = ""
    completed: bool = False
    assigned_to: Optional[int] = None
    project_id: Optional[int] = None
    created_at: datetime = datetime.now()


class User(BaseModel):
    id: int
    username: str
    email: str
    role: str = "member"


class Project(BaseModel):
    id: int
    name: str
    description: str = ""
    owner_id: int
    created_at: datetime = datetime.now()


class Comment(BaseModel):
    id: int
    task_id: int
    user_id: int
    content: str
    created_at: datetime = datetime.now()
""")

    # Add CRUD routes to app.py
    (workspace / "api" / "app.py").write_text("""\
from flask import Flask, jsonify, request
from flask_cors import CORS

from models import Task, User, Project, Comment
from auth import require_auth

app = Flask(__name__)
CORS(app)


@app.route("/")
def index():
    return jsonify({"service": "taskapp-api", "version": "1.0"})


@app.route("/health")
def health():
    return jsonify({"status": "ok"})


@app.route("/tasks", methods=["GET"])
def list_tasks():
    return jsonify({"tasks": []})


@app.route("/tasks", methods=["POST"])
@require_auth
def create_task():
    data = request.get_json()
    return jsonify({"task": data}), 201


@app.route("/tasks/<int:task_id>", methods=["GET"])
def get_task(task_id):
    return jsonify({"task": {"id": task_id}})


@app.route("/tasks/<int:task_id>", methods=["PUT"])
@require_auth
def update_task(task_id):
    data = request.get_json()
    return jsonify({"task": data})


@app.route("/tasks/<int:task_id>", methods=["DELETE"])
@require_auth
def delete_task(task_id):
    return jsonify({"deleted": True})


@app.route("/projects", methods=["GET"])
def list_projects():
    return jsonify({"projects": []})


@app.route("/projects", methods=["POST"])
@require_auth
def create_project():
    data = request.get_json()
    return jsonify({"project": data}), 201


if __name__ == "__main__":
    app.run(debug=True)
""")

    repo.seal(
        summary="backend: task CRUD routes + Project/Comment models",
        agent_id="backend-dev",
        agent_type="agent",
        spec_id="backend",
        status="in-progress",
    )

    return ["api/models.py", "api/app.py"]


def frontend_dev(workspace: Path, repo: Any) -> list[str]:
    """Frontend developer: adds TaskList component, task API client.

    Modifies: web/src/App.tsx, web/src/api.ts
    Creates: web/src/components/TaskList.tsx
    """

    # Create TaskList component
    components_dir = workspace / "web" / "src" / "components"
    components_dir.mkdir(parents=True, exist_ok=True)

    (components_dir / "TaskList.tsx").write_text("""\
import React, { useEffect, useState } from 'react';
import { fetchTasks, Task } from '../api';

interface TaskListProps {
  projectId?: number;
}

export function TaskList({ projectId }: TaskListProps) {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetchTasks(projectId).then(data => {
      setTasks(data);
      setLoading(false);
    });
  }, [projectId]);

  if (loading) return <div>Loading tasks...</div>;

  return (
    <div className="task-list">
      <h2>Tasks</h2>
      {tasks.length === 0 ? (
        <p>No tasks yet.</p>
      ) : (
        <ul>
          {tasks.map(task => (
            <li key={task.id}>
              <input type="checkbox" checked={task.completed} readOnly />
              <span>{task.title}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
""")

    # Update App.tsx with TaskList route
    (workspace / "web" / "src" / "App.tsx").write_text("""\
import React from 'react';
import { BrowserRouter, Route, Routes } from 'react-router-dom';
import { TaskList } from './components/TaskList';

function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<div>TaskApp</div>} />
        <Route path="/tasks" element={<TaskList />} />
      </Routes>
    </BrowserRouter>
  );
}

export default App;
""")

    # Update api.ts with task API functions
    (workspace / "web" / "src" / "api.ts").write_text("""\
import axios from 'axios';

const API = axios.create({ baseURL: '/api' });

export interface Task {
  id: number;
  title: string;
  description: string;
  completed: boolean;
}

export async function fetchHealth() {
  const { data } = await API.get('/health');
  return data;
}

export async function fetchTasks(projectId?: number) {
  const params = projectId ? { project_id: projectId } : {};
  const { data } = await API.get('/tasks', { params });
  return data.tasks as Task[];
}

export async function createTask(task: Partial<Task>) {
  const { data } = await API.post('/tasks', task);
  return data.task as Task;
}

export async function updateTask(id: number, updates: Partial<Task>) {
  const { data } = await API.put(`/tasks/${id}`, updates);
  return data.task as Task;
}

export async function deleteTask(id: number) {
  await API.delete(`/tasks/${id}`);
}
""")

    repo.seal(
        summary="frontend: TaskList component + task API client",
        agent_id="frontend-dev",
        agent_type="agent",
        spec_id="frontend",
        status="in-progress",
    )

    return ["web/src/App.tsx", "web/src/api.ts", "web/src/components/TaskList.tsx"]


def rogue_agent(workspace: Path, repo: Any) -> list[str]:
    """Rogue agent: intentionally works outside its declared scope.

    Declared scope is web/src/* but modifies api/auth.py — this should
    trigger a scope warning (not hard rejection, since enforce_scope
    defaults to false).
    """

    # Modify auth.py — this is OUTSIDE rogue's declared scope (web/src/*)
    (workspace / "api" / "auth.py").write_text("""\
from functools import wraps
import hashlib
import secrets


# Rogue agent added JWT-like auth — outside declared scope!
AUTH_SECRET = secrets.token_hex(32)


def require_auth(f):
    \"\"\"Decorator to require authentication.\"\"\"
    @wraps(f)
    def decorated(*args, **kwargs):
        # TODO: implement real auth check
        return f(*args, **kwargs)
    return decorated


def hash_password(password: str) -> str:
    \"\"\"Hash a password with salt.\"\"\"
    salt = secrets.token_hex(16)
    hashed = hashlib.sha256(f"{salt}{password}".encode()).hexdigest()
    return f"{salt}:{hashed}"


def verify_password(password: str, stored: str) -> bool:
    \"\"\"Verify a password against stored hash.\"\"\"
    salt, hashed = stored.split(":")
    return hashlib.sha256(f"{salt}{password}".encode()).hexdigest() == hashed
""")

    repo.seal(
        summary="rogue: added auth helpers (outside declared scope)",
        agent_id="rogue-agent",
        agent_type="agent",
        spec_id="rogue",
        status="in-progress",
    )

    return ["api/auth.py"]
