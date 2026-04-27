"""Run the full regression suite.

Usage:
    python -m tests.regress              # full matrix
    python -m tests.regress --smoke      # quick subset
    python -m tests.regress -k symlink   # filter by name regex
    python -m tests.regress --verbose    # print failure details inline
    python -m tests.regress --keep       # keep work dirs for inspection

Environment overrides:
    RSYNC_C=...        # path to C rsync (default: rsync)
    RSYNC_RS=...       # path to rsync-rs   (default: rsync-rs)
    RSYNC_WRAPPER=...  # SSH-replacement script (default: /usr/local/bin/wrapper)
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path

from .harness import Runner, ScenarioContext
from .scenarios import all_scenarios, smoke_scenarios, filter_scenarios
from .cli_checks import all_cli_checks


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="rsync-rs regression test runner")
    p.add_argument("--smoke", action="store_true", help="run the small smoke set only")
    p.add_argument("--cli-only", action="store_true", help="only run CLI/protocol checks")
    p.add_argument("--no-cli", action="store_true", help="skip CLI/protocol checks")
    p.add_argument("-k", "--filter", default=None, help="regex filter on scenario names")
    p.add_argument("-v", "--verbose", action="store_true", help="print failure details")
    p.add_argument("--keep", action="store_true", help="keep work dirs after run")
    p.add_argument("--work-dir", default=None, help="override scratch directory")
    p.add_argument("-j", "--jobs", type=int, default=0,
                   help="parallel job count (default: cpu_count)")
    p.add_argument("--timeout", type=float, default=5.0,
                   help="default per-scenario timeout in seconds (default: 5)")
    args = p.parse_args(argv)

    ctx = ScenarioContext(
        rsync_c=os.environ.get("RSYNC_C", "rsync"),
        rsync_rs=os.environ.get("RSYNC_RS", "rsync-rs"),
        wrapper=os.environ.get("RSYNC_WRAPPER", "/usr/local/bin/wrapper"),
        timeout_s=args.timeout,
    )

    work_dir = Path(args.work_dir) if args.work_dir else Path(tempfile.mkdtemp(prefix="rsyncrs-regress-"))
    runner = Runner(ctx=ctx, work_dir=work_dir, verbose=args.verbose,
                    filter_re=re.compile(args.filter) if args.filter else None,
                    jobs=args.jobs)

    scenarios = []
    if not args.cli_only:
        scenarios.extend(smoke_scenarios() if args.smoke else all_scenarios())
    if not args.no_cli and not args.smoke:
        scenarios.extend(all_cli_checks())

    if args.filter:
        scenarios = filter_scenarios(scenarios, args.filter)

    rc = runner.run(scenarios)

    if not args.keep:
        shutil.rmtree(work_dir, ignore_errors=True)
    else:
        print(f"\nWork dir kept at: {work_dir}")
    return rc


if __name__ == "__main__":
    sys.exit(main())
