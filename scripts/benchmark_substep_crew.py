#!/usr/bin/env python3
"""Summarize real run timings, including failed attempts; never invoke models."""
from __future__ import annotations
import argparse
from collections import defaultdict
import json
from pathlib import Path
import statistics


def records(run: Path):
    metrics = run / 'metrics.jsonl'
    if metrics.exists():
        for line in metrics.read_text().splitlines():
            if line.strip():
                yield json.loads(line)
        return
    # Old runs retain Claude counters but no normalized stage metrics.
    for path in sorted(run.glob('*.final.json')):
        try:
            value = json.loads(path.read_text())
        except (ValueError, UnicodeError):
            continue
        if not isinstance(value, dict) or 'duration_ms' not in value:
            continue
        yield {'stage': path.name.split('-', 2)[-1].removesuffix('.final.json'),
               'seconds': value['duration_ms'] / 1000,
               'error': value.get('api_error') or value.get('is_error'),
               'metadata': {'usage': value.get('usage', {})}}


def summarize(run: Path):
    stages = defaultdict(list)
    errors = published = 0
    for item in records(run):
        if 'seconds' in item:
            stages[item['stage']].append(item['seconds'])
        errors += bool(item.get('error') or item.get('ok') is False)
        published += bool(item.get('applied'))
    return {'run': str(run), 'completed': (run / 'completion.json').exists(),
            'stopped_with_error': (run / 'error.txt').exists(),
            'published_edits': published, 'failed_events': errors,
            'stages': {stage: {'calls': len(times), 'total_seconds': round(sum(times), 3),
                                'median_seconds': round(statistics.median(times), 3)}
                       for stage, times in stages.items()}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('runs', nargs='*', type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1] / '.agents/runs'
    runs = args.runs or sorted(p for p in root.glob('*') if p.is_dir() and not p.name.startswith('.'))
    for run in runs:
        print(json.dumps(summarize(run), ensure_ascii=False))


if __name__ == '__main__':
    main()
