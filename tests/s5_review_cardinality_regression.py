#!/usr/bin/env python3
"""New bounded regression of source review cardinality guards; reuse the existing step runner."""
import argparse
import json
import os
from pathlib import Path

import s5_current_regression_live as runner

runner.PACK = runner.ROOT / "docs/refactor/s5/review-cardinality-regression-20260927"
runner.OWNER = "s5-review-cardinality-20260927"
runner.DEPENDENCIES += ("tests/s5_review_cardinality_regression.py",)

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
