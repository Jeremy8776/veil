"""Veil roadmap session CLI and read-only, loopback-only live preview."""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import errno
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
from urllib.request import urlopen
from urllib.parse import urlsplit, unquote

from roadmap_state import ROOT, STATE, STATUSES, atomic_write, definitions, mutate, now, read_state, set_capability

RUNTIME = ROOT / '.roadmap-runtime.json'
WORKSPACE = hashlib.sha256(str(ROOT).encode()).hexdigest()[:20]
RENDER_LOCK = threading.Lock()
rendered_revision = None


def document_revision():
    return hashlib.sha256(b''.join(p.read_bytes() for p in (
        ROOT / 'docs/ROADMAP.md', ROOT / 'tools/render-roadmap.py'))).hexdigest()


def render():
    subprocess.run([sys.executable, str(ROOT / 'tools/render-roadmap.py')], cwd=ROOT, check=True,
                   stdout=subprocess.DEVNULL, timeout=30)


def refresh():
    global rendered_revision
    revision = (document_revision(), read_state()['revision'])
    with RENDER_LOCK:
        if revision != rendered_revision:
            render()
            rendered_revision = revision


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        expected = f'127.0.0.1:{self.server.server_port}'
        if self.headers.get('Host') != expected:
            return self.send_error(403, 'Loopback host required')
        if self.headers.get('Origin') not in (None, f'http://{expected}'):
            return self.send_error(403, 'Same-origin preview required')
        route = unquote(urlsplit(self.path).path)
        if route in ('/api/state', '/api/health'):
            if route == '/api/state':
                refresh()
            payload = {'service': 'veil-roadmap', 'workspace': WORKSPACE,
                       'document_revision': document_revision(), 'state': read_state()}
            return self.respond(json.dumps(payload).encode(), 'application/json')
        if route in ('/', '/roadmap.html'):
            refresh()
            return self.respond((ROOT / 'docs/roadmap.html').read_bytes(), 'text/html; charset=utf-8')
        # Serve only documentation links, never source/config, .git or arbitrary files.
        relative = route.lstrip('/')
        candidate = (ROOT / 'docs' / relative).resolve()
        if route == '/mobile/README.md':
            candidate = (ROOT / 'mobile/README.md').resolve()
        if route == '/AGENTS.md':
            candidate = (ROOT / 'AGENTS.md').resolve()
        allowed = {p.resolve() for p in (ROOT / 'docs').glob('*.md')}
        allowed.add((ROOT / 'mobile/README.md').resolve())
        allowed.add((ROOT / 'AGENTS.md').resolve())
        if candidate in allowed:
            return self.respond(candidate.read_bytes(), 'text/plain; charset=utf-8')
        self.send_error(404)

    def respond(self, data, content_type):
        self.send_response(200)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Referrer-Policy', 'no-referrer')
        self.send_header('X-Frame-Options', 'DENY')
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass


def health(url):
    try:
        target = urlsplit(url)
        if target.scheme != 'http' or target.hostname != '127.0.0.1' or target.port not in range(8766, 8786):
            return False
        with urlopen(url + '/api/health', timeout=.3) as response:
            data = json.load(response)
        return data.get('service') == 'veil-roadmap' and data.get('workspace') == WORKSPACE
    except (OSError, ValueError):
        return False


def start_server():
    if RUNTIME.exists():
        info = json.loads(RUNTIME.read_text(encoding='utf-8'))
        if health(info['url']):
            return info['url']
    for port in range(8766, 8786):
        url = f'http://127.0.0.1:{port}'
        if health(url):
            atomic_write(RUNTIME, json.dumps({'url': url}))
            return url
        try:
            with socket.socket() as probe:
                probe.bind(('127.0.0.1', port))
        except OSError as error:
            if error.errno == errno.EADDRINUSE or getattr(error, 'winerror', None) == 10048:
                continue
            raise RuntimeError('Local preview binding was denied; use a permitted local execution context') from error
        log = ROOT / '.roadmap-server.log'
        with log.open('ab') as output:
            options = {'creationflags': subprocess.CREATE_NO_WINDOW} if sys.platform == 'win32' else {'start_new_session': True}
            process = subprocess.Popen([sys.executable, str(Path(__file__)), 'serve', '--port', str(port)],
                                       cwd=ROOT, stdin=subprocess.DEVNULL, stdout=output, stderr=output, **options)
        for _ in range(30):
            if health(url):
                atomic_write(RUNTIME, json.dumps({'url': url}))
                return url
            if process.poll() is not None:
                break
            time.sleep(.1)
        if process.poll() is None:
            process.terminate()
            raise RuntimeError('The local preview did not answer; inspect .roadmap-server.log or use a permitted execution context')
    raise RuntimeError('Could not start the local roadmap preview; see .roadmap-server.log')


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    sub = result.add_subparsers(dest='command', required=True)
    sub.add_parser('status')
    sub.add_parser('render')
    start = sub.add_parser('start')
    start.add_argument('--session', required=True)
    start.add_argument('--title', required=True)
    session = sub.add_parser('session')
    session.add_argument('--session', required=True)
    session.add_argument('--status', choices=['active', 'idle', 'ended'], required=True)
    session.add_argument('--note', required=True)
    update = sub.add_parser('update')
    update.add_argument('capability', choices=list(definitions()))
    update.add_argument('--status', choices=STATUSES, required=True)
    update.add_argument('--note', required=True)
    update.add_argument('--evidence', action='append', default=[])
    update.add_argument('--session')
    nxt = sub.add_parser('next')
    nxt.add_argument('capability', choices=list(definitions()))
    nxt.add_argument('--title', required=True)
    nxt.add_argument('--why', required=True)
    nxt.add_argument('--check', action='append', required=True)
    nxt.add_argument('--session')
    serve = sub.add_parser('serve')
    serve.add_argument('--port', type=int, default=8766)
    return result


def main():
    args = parser().parse_args()
    if args.command == 'serve':
        refresh()
        ThreadingHTTPServer(('127.0.0.1', args.port), Handler).serve_forever()
        return
    if args.command == 'status':
        print(json.dumps(read_state(), indent=2, ensure_ascii=False))
        return
    if args.command == 'render':
        render()
        print(ROOT / 'docs/roadmap.html')
        return
    if args.command == 'start':
        def apply(state):
            previous = state['sessions'].get(args.session, {})
            state['sessions'][args.session] = {'title': args.title, 'status': 'active',
                                              'started_at': previous.get('started_at', now()),
                                              'updated_at': now(), 'note': 'Reading the roadmap and current evidence.'}
        mutate(apply, f'Session opened: {args.title}', args.session)
        render()
        print(start_server() + '/#live')
    elif args.command == 'session':
        def apply(state):
            if args.session not in state['sessions']:
                raise ValueError('Start the session before updating it')
            state['sessions'][args.session].update(status=args.status, note=args.note, updated_at=now())
        mutate(apply, args.note, args.session)
        render()
    elif args.command == 'update':
        mutate(lambda state: set_capability(state, args.capability, args.status, args.note, args.evidence),
               f'{args.capability}: {args.status} — {args.note}', args.session)
        render()
    elif args.command == 'next':
        def apply(state):
            if state['capabilities'][args.capability]['status'] in ('done', 'blocked'):
                raise ValueError('Next step cannot be completed or blocked')
            state['next_step'] = {'capability': args.capability, 'title': args.title,
                                  'why': args.why, 'checks': args.check}
        mutate(apply, f'Next step: {args.capability} — {args.title}', args.session)
        render()


if __name__ == '__main__':
    try:
        main()
    except (ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error))
