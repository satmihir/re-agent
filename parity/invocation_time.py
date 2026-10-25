"""Invocation elapsed bounds use monotonic clocks; wall anchors retain their raw values."""

import math


def validate_bounds(invocation):
    """Return ordered elapsed-clock endpoints, accepting unchanged legacy evidence."""
    wall = tuple(invocation.get(key) for key in ('started', 'ended'))
    if any(type(value) not in (int, float) or not math.isfinite(value) for value in wall):
        raise ValueError('invalid replay process bounds')
    monotonic = ('monotonic_started', 'monotonic_ended')
    has_monotonic = any(key in invocation for key in monotonic)
    bounds = tuple(invocation.get(key) for key in monotonic) if has_monotonic else wall
    if (any(type(value) not in (int, float) or not math.isfinite(value) for value in bounds)
            or bounds[0] > bounds[1] or (has_monotonic and not math.isfinite(bounds[1] - bounds[0]))):
        raise ValueError('invalid replay process bounds')
    return bounds


def elapsed(invocation):
    """Return elapsed seconds without falling back from malformed monotonic evidence."""
    started, ended = validate_bounds(invocation)
    return ended - started
