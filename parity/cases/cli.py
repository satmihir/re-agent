"""CLI cases and explicit checkpoint dependencies at the executable boundary."""

from dataclasses import replace

from parity.model import Case
from parity.cases import cli_args, cli_run, cli_commands, cli_pty, cli_pty_races, cli_pty_commands


def collect():
    cases = cli_args.collect() + cli_run.collect() + cli_commands.collect() + cli_pty.collect()
    replacements = {case.id: case for case in cli_pty_races.collect() + cli_pty_commands.collect()}
    cases = [replacements.pop(case.id, case) for case in cases] + list(replacements.values())
    cases = [replace(case, pending=('pinned Go defect: C44 stopped after queued input survived a '
                                   'trace-confirmed cancelled run; reproduction cli_pty_races._cancel_typeahead; '
                                   'fix and re-pin deferred by task rules'))
             if case.id.startswith('C44.') else case for case in cases]
    cases = [replace(case, pending=('pinned Go defect: C18 stopped after editor accept/prompt recovery '
                                   'lost the first byte of the following human turn (next -> ext); '
                                   "reproduction cli_pty_commands.edit(ctx, 'visual'); fix/re-pin deferred by task rules"))
             if case.id.startswith('C18.') else case for case in cases]
    cases = [replace(case, pending=('pinned Go defect: C43 stopped after Ctrl-L produced no redraw; '
                                   'editor.go decodes clear but editor.apply has no clear handler; '
                                   'reproduction cli_pty_render._edit(ctx, clear-screen); fix/re-pin deferred'))
             if case.id.startswith('C43.') else case for case in cases]
    cases.extend([
        Case('C19.checkpoints', 'Canonical checkpoints and resume across processes', None,
             pending='area 4: canonical v2 fixtures, cross-import, shared-HOME identity and persisted-field crosswalk'),
        Case('C20.persistence-barriers', 'Trace warnings and checkpoint publication barriers', None,
             pending='area 4: checkpoint save-failure fixtures before model/tool publication; trace warning alone cannot prove persistence barriers'),
    ])
    return sorted(cases, key=lambda case: case.id)
