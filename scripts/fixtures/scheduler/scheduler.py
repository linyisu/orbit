"""Resource-constrained scheduler. Public API must remain unchanged."""
from graph import prepare
from model import Run, Task


def schedule(tasks: list[Task], slots: int = 2, memory: int = 4) -> list[Run]:
    by_id, order, rank = prepare(tasks, slots, memory)
    now = 0
    result = []
    for key in order:
        task = by_id[key]
        result.append(Run(key, now, now + task.duration))
        now += task.duration
    return result
