"""Transactional local work-state for Veil's roadmap. No network or code inspection."""
from contextlib import contextmanager
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
STATE = ROOT / 'docs/roadmap-state.json'
STATUSES = ('planned', 'in_progress', 'blocked', 'done')
PRIORITY = ['AND-02', 'SDK-01', 'TRUST-02', 'DIAG-01', 'LOCAL-01', 'TEST-01',
            'MOB-01', 'MOB-02', 'INVITE-01', 'DIST-01', 'TRUST-01', 'SDK-02',
            'DATA-01', 'NET-01', 'NET-02', 'NET-03', 'OPS-01', 'DESK-01',
            'ID-01', 'BIND-01', 'EMBED-01', 'MEDIA-01', 'GROUP-01', 'READY-01']


def now():
    return datetime.now(timezone.utc).isoformat(timespec='seconds')


def definitions(root=ROOT):
    source = (root / 'docs/ROADMAP.md').read_text(encoding='utf-8')
    backlog = source.split('## Capability backlog\n', 1)[1].split('\n## ', 1)[0]
    parts = re.split(r'^### (.+)\n', backlog, flags=re.M)
    result = {}
    for title, body in zip(parts[1::2], parts[2::2]):
        cap, name = title.split(' ', 1)
        dep = re.search(r'\*\*Depends on:\*\*\s*(.*?)(?:\n\n|$)', body, re.S)
        result[cap] = {'title': name, 'dependencies': re.findall(r'\b[A-Z]+-\d+\b', dep[1])}
    # READY-01 names the mandatory milestone set in prose rather than repeating its IDs.
    result['READY-01']['dependencies'] = [k for k in PRIORITY if k != 'READY-01']
    return result


def scaffold():
    stamp = now()
    return {
        'schema_version': 1, 'revision': 1, 'updated_at': stamp,
        'next_step': {
            'capability': 'AND-02',
            'title': 'Review the Android integration and make its build reproducible',
            'why': 'Capture the existing phone/desktop work before extracting the shared SDK. The 5G connection has already been user-tested.',
            'checks': ['Review the local Android/shared-library diff and native lifetime boundaries.',
                       'Record findings and a small integration/PR plan.',
                       'Verify a clean-checkout Android build and preserve desktop CI.'],
        },
        'capabilities': {k: {'status': 'planned', 'note': 'Full capability acceptance gate remains open.',
                             'evidence': [], 'updated_at': stamp} for k in definitions()},
        'sessions': {}, 'activity': [],
    }


def read_state(path=STATE):
    if not path.exists():
        return scaffold()
    state = json.loads(path.read_text(encoding='utf-8'))
    validate(state)
    return state


def validate(state):
    if state.get('schema_version') != 1:
        raise ValueError('Unsupported roadmap state schema')
    known = definitions()
    if set(state['capabilities']) != set(known):
        raise ValueError('Capability IDs changed; migrate the roadmap state explicitly')
    for cap, item in state['capabilities'].items():
        if item['status'] not in STATUSES or not item.get('note', '').strip():
            raise ValueError(f'Invalid status/note for {cap}')
        if item['status'] == 'done' and (not item.get('evidence') or not all(isinstance(x, str) and x.strip() for x in item['evidence'])):
            raise ValueError(f'{cap} needs completion evidence')
    step = state.get('next_step')
    if step and (step['capability'] not in known or not step.get('title') or not step.get('checks')):
        raise ValueError('Next step requires a known capability, title and completion checks')


@contextmanager
def locked(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.with_suffix('.lock').open('a+b') as handle:
        handle.seek(0, 2)
        if handle.tell() == 0:
            handle.write(b'0')
            handle.flush()
        deadline = time.monotonic() + 10
        while True:
            try:
                handle.seek(0)
                if os.name == 'nt':
                    import msvcrt
                    msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.monotonic() > deadline:
                    raise TimeoutError('Another roadmap writer holds the state lock; retry')
                time.sleep(.05)
        try:
            yield
        finally:
            handle.seek(0)
            if os.name == 'nt':
                msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def atomic_write(path, text):
    temp = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    try:
        temp.write_text(text, encoding='utf-8')
        os.replace(temp, path)
    finally:
        temp.unlink(missing_ok=True)


def mutate(callback, note, session=None, path=STATE):
    with locked(path):
        state = read_state(path)
        callback(state)
        state['revision'] += 1
        state['updated_at'] = now()
        if session and session in state['sessions']:
            state['sessions'][session]['updated_at'] = state['updated_at']
        state['activity'] = ([{'at': state['updated_at'], 'session': session, 'note': note}]
                             + state['activity'])[:100]
        validate(state)
        atomic_write(path, json.dumps(state, indent=2, ensure_ascii=False) + '\n')
        return state


def ready_next(state):
    known = definitions()
    completed = {'BASE-01', 'AND-01'} | {k for k, v in state['capabilities'].items() if v['status'] == 'done'}
    for cap in PRIORITY:
        if state['capabilities'][cap]['status'] in ('done', 'blocked'):
            continue
        if not set(known[cap]['dependencies']) <= completed:
            continue
        return {'capability': cap, 'title': known[cap]['title'],
                'why': 'Next eligible core capability in the recorded priority order; full acceptance criteria remain in its detail.',
                'checks': ['Complete the capability acceptance criteria and attach verification evidence.']}
    return None


def set_capability(state, cap, status, note, evidence):
    if cap not in state['capabilities'] or status not in STATUSES:
        raise ValueError('Unknown capability or status')
    if not note.strip():
        raise ValueError('Explain the work state in --note')
    evidence = [value for value in evidence if value.strip()]
    item = state['capabilities'][cap]
    if status == 'done' and not evidence:
        raise ValueError('Marking done requires explicit --evidence for the full acceptance gate')
    item.update(status=status, note=note, updated_at=now())
    item['evidence'] = list(dict.fromkeys(item['evidence'] + evidence))
    if (state.get('next_step') or {}).get('capability') == cap and status in ('done', 'blocked'):
        state['next_step'] = ready_next(state)
