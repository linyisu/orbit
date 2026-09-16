"""Graph preparation for the scheduler. Contains known correctness bugs."""
from model import Task


def prepare(tasks: list[Task], slots: int, memory: int):
    by_id = {task.id: task for task in tasks}
    order = list(by_id)
    rank = {task.id: task.duration for task in tasks}
    return by_id, order, rank
