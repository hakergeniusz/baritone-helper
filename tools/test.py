#!/usr/bin/env python3
"""The standalone suite: format check, clippy and the runtime's own tests."""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(command):
    print(f"$ {' '.join(command)}", flush=True)
    result = subprocess.run(command, cwd=ROOT)
    if result.returncode:
        sys.exit(result.returncode)


run(["cargo", "fmt", "--check"])
run(["cargo", "clippy", "--all-targets", "--", "-D", "warnings"])
run(["cargo", "test", "--locked"])
print("baritone_helper: standalone suite passed")
