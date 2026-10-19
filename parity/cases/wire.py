"""Stable wire inventory shared by capture, candidates and mutation probes."""
from parity.cases import wire_input, wire_decode, wire_routing, wire_endpoints


def collect():
    cases = (wire_input.collect() + wire_decode.collect() + wire_routing.collect()
             + wire_endpoints.collect())
    return sorted(cases, key=lambda case: case.id)
