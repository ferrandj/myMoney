import os

import pytest

from mymoney.config import Settings


@pytest.fixture
def settings(tmp_path, monkeypatch):
    monkeypatch.setenv("MYMONEY_DATA_DIR", str(tmp_path))
    # Cheap Argon2 parameters to keep tests fast.
    monkeypatch.setenv("MYMONEY_ARGON2_MEMORY_KIB", "8192")
    monkeypatch.setenv("MYMONEY_ARGON2_TIME_COST", "1")
    monkeypatch.setenv("MYMONEY_AUTO_SYNC_HOURS", "0")
    return Settings()


PASSWORD = "Correct-Horse-9!"
