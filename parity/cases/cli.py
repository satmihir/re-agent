"""CLI cases and explicit checkpoint dependencies at the executable boundary."""

from parity.model import Case
from parity.cases import cli_args, cli_run, cli_commands, cli_pty, cli_pty_races, cli_pty_commands


def collect():
    cases = cli_args.collect() + cli_run.collect() + cli_commands.collect() + cli_pty.collect()
    replacements = {case.id: case for case in cli_pty_races.collect() + cli_pty_commands.collect()}
    cases = [replacements.pop(case.id, case) for case in cases] + list(replacements.values())
    cases.extend([
        Case('C19.checkpoints', 'Canonical checkpoints and resume across processes', None,
             pending='area 4: canonical v2 fixtures, cross-import, shared-HOME identity and persisted-field crosswalk'),
        Case('C20.persistence-barriers', 'Trace warnings and checkpoint publication barriers', None,
             pending='area 4: checkpoint save-failure fixtures before model/tool publication; trace warning alone cannot prove persistence barriers'),
    ])
    return sorted(cases, key=lambda case: case.id)
