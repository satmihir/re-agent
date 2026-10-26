"""Shared scripted-agent ordering, outside the provider's matcher lock."""

import re
import time

from parity.observe_agents_effects import TraceBarrier, TraceTimeout


def task_actor(text):
    """Extract fixture routing data from a task or follow-up message."""
    match = re.search(r'(?:^|\n)(?:CHILD_|agent-case:)([a-z0-9-]+)', text)
    return match[1] if match else None


class AgentOrder:
    """Hold each child Run's replies until its parent's next joining wait starts."""

    def __init__(self, ctx):
        self.ctx = ctx
        self.calls = {}
        self.actions = {}
        self.names = {}
        self.runs = {}

    def reply(self, who, calls=(), terminal=False, barrier=None, unordered=None):
        """Register scripts without doing any I/O or waiting in a selector."""
        sequence = self.calls.setdefault(who, [])
        for call_id, name, args in calls:
            sequence.append((call_id, name, args))
            if not isinstance(args, dict):
                continue
            target = args.get('name') or args.get('id')
            child = None
            if name == 'spawn':
                child = task_actor(args.get('task', ''))
                if child and target:
                    self.names[who, target] = child
            if name == 'send':
                child = task_actor(args.get('message', '')) or self.names.get((who, target))
            if child:
                self.actions.setdefault(child, []).append((who, len(sequence) - 1, target))
        run = self.runs.get(who, 0)
        if terminal:
            self.runs[who] = run + 1
        if who == 'root' or unordered is not None:
            if unordered is not None:
                assert isinstance(unordered, str) and unordered.strip(), 'unordered replies need a reason'
            return barrier
        return ChildReply(self, who, run, barrier)


def ordering(ctx):
    """Share registration between queue calls and Script instances in one fixture."""
    if not hasattr(ctx, '_agent_order'):
        ctx._agent_order = AgentOrder(ctx)
    return ctx._agent_order


class ChildReply:
    """A provider response barrier; its waits never run under the matcher lock."""

    def __init__(self, order, who, run, explicit):
        self.order, self.who, self.run, self.explicit = order, who, run, explicit
        self.ready = False

    def wait(self, timeout=None):
        deadline = None if timeout is None else time.monotonic() + timeout
        if not self.ready:
            actions = self.order.actions.get(self.who, [])
            assert self.run < len(actions), ('missing scripted parent action', self.who, self.run)
            def joined(records):
                admitted = []
                for parent, index, target in actions:
                    calls = self.order.calls[parent]
                    launch = calls[index][0]
                    owners = {'root'} if parent == 'root' else {
                        record['data']['id'] for record in records if record['type'] == 'agent.spawn'
                        and task_actor(record['data'].get('task', '')) == parent}
                    for record in records:
                        if (record['type'] == 'tool.finished' and record['data'].get('call_id') == launch
                                and record.get('agent_id', 'root') in owners
                                and record['data']['outcome'].get('ok')):
                            admitted.append((record, calls, index, target))
                if len(admitted) <= self.run:
                    return False
                start, calls, index, target = admitted[self.run]
                launch = calls[index][0]
                owner = start.get('agent_id', 'root')
                run_id = start.get('run_id')
                ids = {target, start['data']['outcome'].get('data', {}).get('id')}
                prior_waits = []
                for call_id, name, args in calls[index + 1:]:
                    if name != 'wait' or not isinstance(args, dict) or 'timeout' in args:
                        continue
                    if ids.intersection(args.get('ids', [])):
                        break
                    prior_waits.append(call_id)
                else:
                    return False
                parent_records = [record for record in records
                                  if record.get('agent_id', 'root') == owner
                                  and record.get('run_id') == run_id]
                finished = {record['data'].get('call_id') for record in parent_records
                            if record['type'] == 'tool.finished'
                            and record['data']['outcome'].get('ok') is True}
                # run.finished precedes the tree callback; a successful parent wait does not.
                return all(wait in finished for wait in prior_waits) and any(
                    record['type'] == 'tool.started' and record['data'].get('call_id') == call_id
                    for record in parent_records)

            try:
                with TraceBarrier(self.order.ctx.artifacts) as witness:
                    remaining = 12 if deadline is None else max(0, deadline - time.monotonic())
                    witness.wait(joined, self.who + ' parent joining wait', timeout=remaining)
            except TraceTimeout:
                return False
            self.ready = True
        if self.explicit is None:
            return True
        event = (self.order.ctx.server.barrier(self.explicit)
                 if isinstance(self.explicit, str) else self.explicit)
        remaining = None if deadline is None else max(0, deadline - time.monotonic())
        return event.wait(remaining)
