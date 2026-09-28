#!/usr/bin/env python3
"""Same-version 40-operation development regression with explicitly budgeted coverage repair."""
import argparse
from functools import partial
import json
import os
from pathlib import Path

import s5_current_regression_live as runner

runner.PACK = runner.ROOT / "docs/refactor/s5/source-repair-calendar-regression-20260928"
runner.OWNER = "s5-source-repair-calendar-20260928"
runner.DEPENDENCIES += ("tests/s5_source_repair_calendar_regression.py",)
runner.static_check = partial(runner.static_check, allow_coverage_repair=True)

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("check", "preflight", "step", "status"))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    if args.phase == "check":
        import s5_current_regression_check
        s5_current_regression_check.main()
    else:
        assert args.output is not None, "--output required"
        output = args.output.resolve()
        if args.phase == "preflight":
            runner.preflight(output)
        elif args.phase == "step":
            runner.step(output)
        else:
            print(json.dumps(runner.summary(runner.read(output / "report.json")), ensure_ascii=False))
