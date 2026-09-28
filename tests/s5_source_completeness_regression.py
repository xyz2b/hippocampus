#!/usr/bin/env python3
"""New bounded regression of source completeness and temporal review context; reuse the existing step runner."""
import argparse
import json
import os
from pathlib import Path

import s5_current_regression_live as runner

runner.PACK = runner.ROOT / "docs/refactor/s5/source-completeness-regression-20260928"
runner.OWNER = "s5-source-completeness-20260928"
runner.DEPENDENCIES += ("tests/s5_source_completeness_regression.py",)

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
