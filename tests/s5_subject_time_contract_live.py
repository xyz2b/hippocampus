#!/usr/bin/env python3
"""Select an isolated acceptance pack while reusing the existing guarded runners."""
import argparse
import json
import os
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("suite", choices=("specialist", "regression"))
    parser.add_argument("phase", choices=("check", "preflight", "step", "status"))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--pack-prefix", choices=("subject-time-contract", "review-evidence", "review-source", "extraction-correction"),
                        default="subject-time-contract")
    args = parser.parse_args()
    os.umask(0o077)
    if args.suite == "specialist":
        import s5_source_repair_calendar_live as delegate
        executor = delegate.specialist
        check = delegate.check
        owner = f"s5-{args.pack_prefix}-specialist-20260928"
    else:
        import s5_source_repair_calendar_regression as delegate
        import s5_current_regression_check
        executor = delegate.runner
        check = s5_current_regression_check.main
        owner = f"s5-{args.pack_prefix}-20260928"
    runner = delegate.runner
    delegate.PACK = runner.PACK = runner.ROOT / f"docs/refactor/s5/{args.pack_prefix}-{args.suite}-20260928"
    delegate.OWNER = runner.OWNER = owner
    runner.DEPENDENCIES += ("tests/s5_subject_time_contract_live.py",)
    correction = args.pack_prefix == "extraction-correction"
    if correction and args.suite == "regression":
        from functools import partial
        runner.static_check = partial(runner.static_check, allow_extraction_repair=True)
    if args.phase == "check":
        if correction and args.suite == "specialist":
            from unittest.mock import patch
            old_pack = runner.ROOT / "docs/refactor/s5/review-source-specialist-20260928"
            old_owner = "s5-review-source-specialist-20260928"
            with patch.object(delegate, "PACK", old_pack), patch.object(runner, "PACK", old_pack), \
                    patch.object(delegate, "OWNER", old_owner), patch.object(runner, "OWNER", old_owner):
                check()
            executor.plan()
        else:
            check()
        if correction:
            runner.check_extraction_correction_guards(
                executor.inspect_repair_validations if args.suite == "specialist" else None)
    else:
        assert args.output is not None, "--output required"
        output = args.output.resolve()
        if args.suite == "regression" and args.phase == "status":
            print(json.dumps(runner.summary(runner.read(output / "report.json")), ensure_ascii=False))
        else:
            getattr(executor, args.phase)(output)


if __name__ == "__main__":
    main()
