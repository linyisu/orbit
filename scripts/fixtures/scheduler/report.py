from model import Run, Task
from scheduler import schedule


def summarize(tasks: list[Task], slots: int = 2, memory: int = 4) -> dict:
    runs = schedule(tasks, slots, memory)
    return {
        "makespan": sum(run.end - run.start for run in runs),
        "busy_time": sum(task.duration for task in tasks),
        "peak_memory": sum(task.memory for task in tasks),
        "runs": [{"id": run.id, "start": run.start, "end": run.end} for run in runs],
    }
