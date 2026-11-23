"""Regression checks for shared-state integrity and evidence-based next-step selection."""
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import tempfile
import unittest

from roadmap_state import mutate, read_state, scaffold, set_capability


class RoadmapStateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'roadmap-state.json'
        self.path.write_text(json.dumps(scaffold()), encoding='utf-8')

    def test_concurrent_writers_preserve_each_others_activity(self):
        def update(index):
            mutate(lambda state: None, f'writer-{index}', path=self.path)
        with ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(update, range(32)))
        state = read_state(self.path)
        self.assertEqual(state['revision'], 33)
        self.assertEqual({x['note'] for x in state['activity']}, {f'writer-{i}' for i in range(32)})

    def test_completion_without_evidence_cannot_change_the_file(self):
        before = self.path.read_bytes()
        with self.assertRaises(ValueError):
            mutate(lambda s: set_capability(s, 'AND-02', 'done', 'Finished', []), 'bad', path=self.path)
        self.assertEqual(self.path.read_bytes(), before)
        with self.assertRaises(ValueError):
            mutate(lambda s: set_capability(s, 'AND-02', 'done', 'Finished', [' ']), 'bad', path=self.path)
        self.assertEqual(self.path.read_bytes(), before)

    def test_completing_next_advances_to_an_eligible_gate(self):
        state = mutate(lambda s: set_capability(s, 'AND-02', 'done', 'Acceptance verified', ['test record']), 'done', path=self.path)
        self.assertEqual(state['next_step']['capability'], 'SDK-01')
        self.assertEqual(state['capabilities']['SDK-01']['status'], 'planned')

    def test_blocked_next_is_visible_and_not_selected_again(self):
        state = mutate(lambda s: set_capability(s, 'AND-02', 'blocked', 'Build tool unavailable', []), 'blocked', path=self.path)
        self.assertEqual(state['capabilities']['AND-02']['status'], 'blocked')
        self.assertEqual(state['next_step']['capability'], 'SDK-01')

    def test_unrelated_update_preserves_next_and_existing_evidence(self):
        mutate(lambda s: set_capability(s, 'SDK-01', 'in_progress', 'API inventory', ['inventory notes']), 'started', path=self.path)
        state = mutate(lambda s: set_capability(s, 'SDK-01', 'blocked', 'Review pending', []), 'waiting', path=self.path)
        self.assertEqual(state['next_step']['capability'], 'AND-02')
        self.assertEqual(state['capabilities']['SDK-01']['evidence'], ['inventory notes'])

    def test_unknown_capability_or_invalid_status_is_rejected(self):
        for cap, status in [('FAKE-01', 'planned'), ('AND-02', 'looks-done')]:
            with self.assertRaises(ValueError):
                mutate(lambda s: set_capability(s, cap, status, 'note', []), 'bad', path=self.path)


if __name__ == '__main__':
    unittest.main()
