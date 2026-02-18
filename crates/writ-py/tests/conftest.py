"""Shared fixtures for writ Python binding tests."""

import os
import tempfile

import pytest
import writ


@pytest.fixture
def tmp_repo(tmp_path):
    """Create a temporary writ repository and return the repo + path."""
    repo = writ.Repository.init(str(tmp_path))
    return repo, tmp_path
