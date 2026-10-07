"""Offline capacity estimate from the public AC2 fixture usage, not a quote."""
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PATH = ROOT / 'end_to_end/ac2-pro-medium-pro-minimal-pro-minimal-r4-compute-low/run.json'


def estimate(mode, planned=None):
    counts = planned if planned is not None else ({'W1':4, 'W3':4, 'W4':4, 'R1':6, 'R4':6} if mode == 'mvp' else {'W1':24})
    groups = {stage:[] for stage in counts}
    for case in json.loads(PATH.read_text())['cases']:
        for call in case.get('calls', []):
            if call.get('stage') in groups and call.get('usage'):
                groups[call['stage']].append(call['usage'])
    total = {token:round(sum(counts[stage] * sum(u.get(token, 0) for u in uses)/len(uses)
                            for stage, uses in groups.items())) for token in ('input_tokens', 'output_tokens')}
    print(f"AC2 usage 均值容量粗估：输入约 {total['input_tokens']}、输出约 {total['output_tokens']} token；非费用或用量上界。")
    if mode == 'w1':
        print('历史容量按 medium 均值估算；M1.5 对照 low 实测均值 3337 输出 token，medium 为 4512，不能当作质量或未来用量保证。')
    else:
        print(f'MVP 未来输出未知；按 {sum(counts.values())} 个新发送槽位和历史均值估算，跳过阶段会减少实际用量，复用不计新发送。')
    return total


if __name__ == '__main__':
    estimate(sys.argv[1], json.loads(sys.argv[2]) if len(sys.argv) > 2 else None)
