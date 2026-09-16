import itertools
import random
import unittest

from graph import prepare
from model import Run, Task
from report import summarize
from scheduler import schedule


class GraphTests(unittest.TestCase):
    def test_critical_path_rank_includes_transitive_successors(self):
        tasks = [Task("a", 2), Task("b", 3, deps=("a",)), Task("c", 7, deps=("a",)), Task("d", 5, deps=("b",))]
        _, order, rank = prepare(tasks, 2, 4)
        self.assertEqual(rank, {"a": 10, "b": 8, "c": 7, "d": 5})
        self.assertLess(order.index("a"), order.index("b"))
        self.assertLess(order.index("b"), order.index("d"))

    def test_deterministic_topological_order(self):
        for tasks in itertools.permutations([Task("z", 1), Task("a", 1), Task("b", 1, deps=("a",))]):
            self.assertEqual(prepare(list(tasks), 2, 4)[1], ["a", "b", "z"])

    def test_invalid_inputs(self):
        cases = [
            ([Task("a", 1), Task("a", 2)], 2, 4),
            ([Task("a", 1, deps=("missing",))], 2, 4),
            ([Task("a", 1, deps=("a",))], 2, 4),
            ([Task("a", 1, deps=("b",)), Task("b", 1, deps=("a",))], 2, 4),
            ([Task("ok", 1), Task("a", 1, deps=("b",)), Task("b", 1, deps=("a",))], 2, 4),
            ([Task("a", 1), Task("b", 1, deps=("a", "a"))], 2, 4),
            ([Task("", 1)], 2, 4), ([Task("a", 0)], 2, 4), ([Task("a", -1)], 2, 4),
            ([Task("a", True)], 2, 4), ([Task("a", 1.5)], 2, 4),
            ([Task("a", 1, 0)], 2, 4), ([Task("a", 1, 5)], 2, 4),
            ([Task("a", 1, True)], 2, 4), ([Task("a", 1, 1.5)], 2, 4),
            ([], 0, 4), ([], True, 4), ([], 2, 0), ([], 2, True),
            ([], 1.5, 4), ([], 2, 1.5),
        ]
        for tasks, slots, memory in cases:
            with self.subTest(tasks=tasks, slots=slots, memory=memory):
                with self.assertRaises(ValueError):
                    schedule(tasks, slots, memory)

    def test_long_chain_does_not_require_python_recursion(self):
        tasks = [Task(str(i), 1, deps=(str(i-1),) if i else ()) for i in range(1200)]
        _, order, rank = prepare(list(reversed(tasks)), 2, 4)
        self.assertEqual(order, [str(i) for i in range(1200)])
        self.assertEqual(rank["0"], 1200)


class SchedulingTests(unittest.TestCase):
    def test_independent_jobs_overlap(self):
        self.assertEqual(schedule([Task("a", 5), Task("b", 3)]), [Run("a", 0, 5), Run("b", 0, 3)])

    def test_dependency_order_not_input_order(self):
        self.assertEqual(schedule([Task("b", 2, deps=("a",)), Task("a", 3)]), [Run("a", 0, 3), Run("b", 3, 5)])

    def test_critical_path_priority(self):
        tasks = [Task("b", 5), Task("c", 1), Task("a", 2), Task("d", 10, deps=("a",))]
        self.assertEqual(schedule(tasks, 1, 4), [Run("a", 0, 2), Run("d", 2, 12), Run("b", 12, 17), Run("c", 17, 18)])

    def test_skip_ready_job_that_does_not_fit(self):
        tasks = [Task("b", 8, 3), Task("a", 7, 2), Task("c", 6, 1)]
        self.assertEqual(schedule(tasks, 2, 4), [Run("b", 0, 8), Run("c", 0, 6), Run("a", 8, 15)])

    def test_all_simultaneous_completions_release_resources(self):
        tasks = [Task("a", 2, 2), Task("b", 2, 2), Task("c", 3, 4, ("a", "b"))]
        self.assertEqual(schedule(tasks), [Run("a", 0, 2), Run("b", 0, 2), Run("c", 2, 5)])

    def test_reconsider_priorities_after_completion(self):
        tasks = [Task("a", 2), Task("b", 8), Task("c", 7, deps=("a",)), Task("d", 1)]
        self.assertEqual(schedule(tasks), [Run("a", 0, 2), Run("b", 0, 8), Run("c", 2, 9), Run("d", 8, 9)])

    def test_lexicographic_ties_and_permutation_invariance(self):
        tasks = [Task("z", 2), Task("a", 2), Task("b", 2)]
        for permutation in itertools.permutations(tasks):
            self.assertEqual(schedule(list(permutation)), [Run("a", 0, 2), Run("b", 0, 2), Run("z", 2, 4)])

    def test_large_duration_uses_events_not_one_tick_at_a_time(self):
        self.assertEqual(schedule([Task("a", 10**12), Task("b", 1, deps=("a",))]), [Run("a", 0, 10**12), Run("b", 10**12, 10**12+1)])

    def test_empty_schedule(self):
        self.assertEqual(schedule([]), [])

    def test_input_is_not_mutated(self):
        tasks = [Task("b", 2), Task("a", 4)]
        original = list(tasks)
        schedule(tasks)
        self.assertEqual(tasks, original)

    def test_seeded_dags_obey_invariants_and_are_deterministic(self):
        rng = random.Random(20260916)
        for _ in range(30):
            tasks = [Task(f"t{i:02d}", rng.randint(1, 8), rng.randint(1, 4), tuple(f"t{j:02d}" for j in range(i) if rng.random() < .18)) for i in range(18)]
            runs = schedule(tasks, 3, 5)
            self.assertEqual(len(runs), len(tasks))
            self.assertEqual({r.id for r in runs}, {t.id for t in tasks})
            self.assertEqual(runs, sorted(runs, key=lambda r: (r.start, r.id)))
            by_id = {r.id: r for r in runs}
            requirements = {t.id: t.memory for t in tasks}
            for task in tasks:
                run = by_id[task.id]
                self.assertGreaterEqual(run.start, 0)
                self.assertEqual(run.end-run.start, task.duration)
                self.assertTrue(all(by_id[dep].end <= run.start for dep in task.deps))
            for time in {r.start for r in runs} | {r.end for r in runs}:
                active = [r for r in runs if r.start <= time < r.end]
                self.assertLessEqual(len(active), 3)
                self.assertLessEqual(sum(requirements[r.id] for r in active), 5)
            rng.shuffle(tasks)
            self.assertEqual(schedule(tasks, 3, 5), runs)


class ReportTests(unittest.TestCase):
    def test_parallel_summary_uses_wall_clock_and_peak_concurrency(self):
        result = summarize([Task("a", 5, 2), Task("b", 2, 1), Task("c", 3, 3, ("a",))])
        self.assertEqual(result["makespan"], 8)
        self.assertEqual(result["busy_time"], 10)
        self.assertEqual(result["peak_memory"], 3)
        self.assertEqual(result["runs"], [{"id":"a", "start":0, "end":5}, {"id":"b", "start":0, "end":2}, {"id":"c", "start":5, "end":8}])

    def test_endpoints_are_half_open(self):
        self.assertEqual(summarize([Task("a", 1, 4), Task("b", 1, 4, ("a",))])["peak_memory"], 4)

    def test_empty_summary(self):
        self.assertEqual(summarize([]), {"makespan":0, "busy_time":0, "peak_memory":0, "runs":[]})


if __name__ == "__main__":
    unittest.main()
