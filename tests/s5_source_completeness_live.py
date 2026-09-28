#!/usr/bin/env python3
"""Bounded source-completeness and temporal-view retest; reuse the reviewed six-source runner."""
import argparse
import os
from pathlib import Path

import s5_temporal_grounding_live as specialist

specialist.runner.PACK = specialist.runner.ROOT / 'docs/refactor/s5/source-completeness-specialist-20260928'
specialist.runner.OWNER = 's5-source-completeness-specialist-20260928'
specialist.runner.DEPENDENCIES += ('tests/s5_source_completeness_live.py',)

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=('check', 'preflight', 'step', 'status'))
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    if args.phase == 'check':
        specialist.check()
    else:
        assert args.output is not None, '--output required'
        getattr(specialist, args.phase)(args.output.resolve())
