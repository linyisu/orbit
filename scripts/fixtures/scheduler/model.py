from dataclasses import dataclass


@dataclass(frozen=True)
class Task:
    id: str
    duration: int
    memory: int = 1
    deps: tuple[str, ...] = ()


@dataclass(frozen=True)
class Run:
    id: str
    start: int
    end: int
