"""Local CI entry point."""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    env = dict(os.environ, LEDGER_FIXTURES=str(ROOT / "tests" / "fixtures"))
    return subprocess.call([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-t", "."], cwd=ROOT, env=env)


if __name__ == "__main__":
    sys.exit(main())
