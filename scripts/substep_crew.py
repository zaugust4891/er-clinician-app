#!/usr/bin/env python3
"""Run one substep through proposed, reviewed, applied, and validated edits.

Usage: python3 scripts/substep_crew.py --dry-run
Code tour: docs/substep-crew-code-tour.md
"""

import sys

if __package__:
    from .substep_crew_lib.cli import main
else:
    from substep_crew_lib.cli import main


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\ninterrupted — working tree left as-is, nothing committed")
        sys.exit(130)
