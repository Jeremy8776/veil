from pathlib import Path
from html import escape
import hashlib
import json
import re

import markdown
from roadmap_state import atomic_write, read_state

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'docs/ROADMAP.md'
OUTPUT = ROOT / 'docs/roadmap.html'
source = SOURCE.read_text(encoding='utf-8')
state = read_state()
state_labels = {'planned': 'Planned gate', 'in_progress': 'In progress', 'blocked': 'Blocked', 'done': 'Complete'}
document_revision = hashlib.sha256(SOURCE.read_bytes() + Path(__file__).read_bytes()).hexdigest()
runtime = ROOT / '.roadmap-runtime.json'
live_url = json.loads(runtime.read_text(encoding='utf-8'))['url'] if runtime.exists() else 'http://127.0.0.1:8766'


def render(text):
    if 'cap_ids' in globals():
        pattern = r'\b(' + '|'.join(re.escape(x) for x in cap_ids) + r')\b'
        text = re.sub(pattern, lambda m: f'[{m[0]}](#{m[0]})', text)
    result = markdown.markdown(text, extensions=['tables', 'toc'])
    return result.replace('<table>', '<div class="table-scroll" tabindex="0" role="region" aria-label="Scrollable reference table"><table>').replace('</table>', '</table></div>')


parts = re.split(r'^## (.+)\n', source, flags=re.M)
sections = dict(zip(parts[1::2], parts[2::2]))
intro = parts[0].split('\n', 1)[1]
capabilities = re.split(r'^### (.+)\n', sections['Capability backlog'], flags=re.M)
cap_ids = [title.split(' ', 1)[0] for title in capabilities[1::2]]
versions = {}
streams = {
    'SDK': 'Engine and integrations', 'BIND': 'Engine and integrations', 'EMBED': 'Engine and integrations',
    'LOCAL': 'Local and invitations', 'INVITE': 'Local and invitations', 'NEAR': 'Local and invitations',
    'TRUST': 'Identity and security', 'ID': 'Identity and security',
    'NET': 'Networks and helpers', 'OPS': 'Networks and helpers',
    'DATA': 'Media and groups', 'GROUP': 'Media and groups', 'MEDIA': 'Media and groups',
    'SCALE': 'Media and groups', 'PERF': 'Media and groups', 'HOST': 'Media and groups',
    'AND': 'Platforms and experience', 'MOB': 'Platforms and experience', 'DIST': 'Platforms and experience',
    'IOS': 'Platforms and experience', 'DESK': 'Platforms and experience',
    'DIAG': 'Evidence and release', 'TEST': 'Evidence and release', 'READY': 'Evidence and release',
    'MSG': 'Future messaging', 'ORG': 'Engine and integrations',
}
cap_html = []
for title, body in zip(capabilities[1::2], capabilities[2::2]):
    cap_id, name = title.split(' ', 1)
    target = re.search(r'\*\*Target:\*\*\s*(.*?)(?=\*\*Depends on:|\n\n)', body, re.S)
    match = re.search(r'\b\d+\.\d+\.\d+\b', target.group(1))
    version = match[0] if match else 'Unassigned'
    versions[cap_id] = version
    stream = streams[cap_id.split('-')[0]]
    work = state['capabilities'][cap_id]
    next_class = ' is-next' if (state.get('next_step') or {}).get('capability') == cap_id else ''
    cap_html.append(f'''<details class="capability{next_class}" id="{cap_id}" data-version="{version}" data-stream="{stream}" data-state="{work['status']}">
      <summary><span class="cap-id">{cap_id}</span><span class="cap-name">{escape(name)}<small class="cap-work-status">{state_labels[work['status']]}</small></span>
      <span class="cap-version">{'Later' if version == 'Unassigned' else version}</span><span class="disclosure" aria-hidden="true">+</span></summary>
      <div class="cap-body"><div class="work-record"><strong>Tracked work state</strong><p class="work-note">{escape(work['note'])}</p><ul class="work-evidence">{''.join('<li>' + escape(x) + '</li>' for x in work['evidence'])}</ul></div><span class="stream-label">{stream}</span>{render(body)}</div></details>''')

milestone_source = sections['Version milestones']
lines = milestone_source.splitlines()
rows = [line for line in lines if line.startswith('|')][2:]
labels = ['Capture the foundation', 'Local first. Everyday usable.', 'A shared engine. More reach.',
          'Veil inside other apps', 'Trustworthy group calls', 'Bring Veil to iPhone', 'A supported shared platform']
milestones = []
for row, label in zip(rows, labels):
    version, outcome, ids, evidence = [cell.strip() for cell in row.strip('|').split('|')]
    bare_version = version.split(',')[0]
    badge = 'Next' if bare_version == '0.1.14' else ('Conditional' if bare_version == '0.6.0' else 'Proposed')
    links = ' '.join(f'<a class="id-link" href="#{cap_id}">{cap_id}</a>' for cap_id in ids.split(', '))
    milestones.append(f'''<article class="milestone" id="v-{bare_version}">
      <div class="version"><span>{bare_version}</span><small>{badge}</small></div>
      <div class="milestone-main"><h3>{label}</h3><p class="outcome">{escape(outcome)}</p>
      <p class="exit"><strong>Complete when</strong> {escape(evidence)}</p><div class="id-links">{links}</div></div>
      </article>''')
milestone_notes = '\n'.join(line for line in lines if not line.startswith('|'))
logo = (ROOT / 'crates/veil-app/assets/veil-logo.svg').read_text(encoding='utf-8')
logo = logo.replace('<svg ', '<svg aria-hidden="true" focusable="false" ')

css = r'''
:root{color-scheme:dark;--page:#17161c;--surface:#222129;--raised:#2e2d37;--line:#3d3b47;--text:#f3f2f7;--muted:#b6b3c5;--accent:#a6a2ff;--purple:#6e6af0;--green:#70d5a4;--gold:#e8c47b;--sans:"Segoe UI Variable Text","Segoe UI",Arial,sans-serif;--display:"Segoe UI Variable Display","Segoe UI",Arial,sans-serif;--mono:Consolas,"Liberation Mono",monospace}
*{box-sizing:border-box}html{scroll-behavior:smooth;scroll-padding-top:28px}body{margin:0;background:var(--page);color:var(--text);font:16px/1.65 var(--sans)}a{color:var(--accent);text-underline-offset:4px}a:hover{color:#d1cfff}button,input,select{font:inherit}button,select{cursor:pointer}button{background:var(--raised);border:1px solid var(--line);color:var(--text);border-radius:7px;padding:9px 15px}button:hover{background:#3a3945}button:focus-visible,a:focus-visible,summary:focus-visible,input:focus-visible,select:focus-visible{outline:3px solid var(--accent);outline-offset:4px}h1,h2,h3,p{margin-top:0}h1,h2,h3{font-family:var(--display);font-weight:600;line-height:1.15}h1{font-size:clamp(46px,6vw,78px);letter-spacing:-.055em;margin-bottom:24px;max-width:800px}h2{font-size:32px;letter-spacing:-.025em;margin-bottom:24px}h3{font-size:23px;letter-spacing:-.02em}p{margin-bottom:18px}strong{font-weight:600}code{font-family:var(--mono);font-size:.88em;background:var(--raised);padding:2px 5px;border-radius:4px}li{margin-bottom:10px}ul{padding-left:23px}section{scroll-margin-top:28px}svg{display:block}button[hidden],[hidden]{display:none!important}
.skip{position:fixed;left:20px;top:-90px;z-index:10;padding:12px;background:var(--surface)}.skip:focus{top:10px}.layout{display:grid;grid-template-columns:228px minmax(0,1fr);max-width:1600px;margin:auto}.sidebar{position:sticky;top:0;height:100vh;padding:36px 25px;border-right:1px solid var(--line);display:flex;flex-direction:column;gap:38px}.brand{display:flex;gap:12px;align-items:center;color:var(--text);text-decoration:none;font-size:25px;font-weight:600;letter-spacing:-1px}.brand svg{width:42px;height:42px}.sidebar nav{display:flex;flex-direction:column;gap:3px}.sidebar nav a{color:var(--muted);text-decoration:none;font-size:14px;padding:10px 12px;border-left:2px solid transparent}.sidebar nav a:hover,.sidebar nav a[aria-current]{color:var(--text);background:var(--surface);border-left-color:var(--accent)}.sidebar-note{margin-top:auto;font-size:12px;color:var(--muted)}.sidebar-note b{display:block;color:var(--text);font-weight:500;font-size:13px;margin-bottom:8px}.sidebar-note p{margin-bottom:8px}.main{min-width:0;padding:0 6.5%}.topline{display:flex;justify-content:space-between;align-items:center;gap:20px;border-bottom:1px solid var(--line);padding:27px 0;font-size:12px;letter-spacing:.08em;text-transform:uppercase;color:var(--muted)}.topline a{text-transform:none;letter-spacing:0;font-size:14px}.hero{padding:62px 0 38px}.eyebrow{display:block;color:var(--accent);font:12px var(--mono);letter-spacing:.12em;text-transform:uppercase;margin-bottom:22px}.hero-lead{font-size:20px;color:var(--muted);max-width:720px;line-height:1.6}.hero-lead strong{color:var(--text);font-weight:500}.hero-bottom{display:flex;gap:20px;align-items:center;flex-wrap:wrap;margin-top:30px}.chip{border:1px solid var(--line);border-radius:30px;padding:5px 12px;font-size:12px;color:var(--muted)}.chip.live{color:var(--green)}.chip.live:before{content:"";display:inline-block;width:6px;height:6px;background:var(--green);border-radius:100%;margin-right:8px}.proof{display:grid;grid-template-columns:140px 1fr;background:#202b29;border:1px solid #3d6252;border-radius:10px;margin:6px 0 60px;overflow:hidden}.proof-label{padding:25px;color:var(--green);font:12px/1.6 var(--mono);border-right:1px solid #3d6252;display:flex;flex-direction:column;justify-content:center}.proof-main{padding:23px 28px}.proof-main h2{font-size:21px;letter-spacing:-.01em;margin-bottom:8px}.proof-main p{font-size:14px;color:#c1d0c9;margin:0}.proof-main a{color:#a2e7c2}.section-top{display:flex;justify-content:space-between;align-items:baseline;gap:16px}.section-top small{font:12px var(--mono);color:var(--muted)}.section-lead{color:var(--muted);max-width:760px}.milestones{margin:30px 0}.milestone{display:grid;grid-template-columns:140px minmax(0,1fr);border-top:1px solid var(--line);padding:29px 0;position:relative}.milestone:last-child{border-bottom:1px solid var(--line)}.version>span{font:26px/1.3 var(--mono);letter-spacing:-1px}.version small{display:block;color:var(--muted);font-size:11px;margin-top:7px}.milestone:first-child .version>span{color:var(--accent)}.milestone:first-child .version small{color:var(--accent)}.milestone-main h3{margin-bottom:10px}.outcome{color:var(--muted);margin-bottom:14px;font-size:15px}.exit{font-size:13px;color:var(--muted);margin:0 0 14px}.exit strong{color:var(--text);margin-right:6px}.id-links{display:flex;flex-wrap:wrap;gap:8px}.id-link{display:inline-flex;border:1px solid var(--line);border-radius:5px;padding:3px 8px;font:11px/1.5 var(--mono);text-decoration:none}.id-link:hover{background:var(--raised)}.notes{font-size:14px;color:var(--muted)}.block{padding:34px 0 42px}.block+.block{border-top:1px solid var(--line)}.toolbar{display:flex;gap:10px;flex-wrap:wrap;margin:25px 0 12px}.field{display:flex;flex-direction:column;gap:6px;flex:1;min-width:180px}.field label{font-size:12px;color:var(--muted)}.field input,.field select{width:100%;padding:11px 12px;background:var(--surface);color:var(--text);border:1px solid var(--line);border-radius:6px;min-height:46px}.field-select{flex:0 1 185px}.toolbar-actions{display:flex;align-items:flex-end;gap:8px}.toolbar-actions button{min-height:46px}.results{color:var(--muted);font-size:12px;display:flex;justify-content:space-between;align-items:center;margin:15px 0}.results button{border:0;background:transparent;font-size:12px;color:var(--accent);padding:4px}.capability{border:1px solid var(--line);border-radius:8px;margin-bottom:10px;background:var(--surface);overflow:hidden}.capability summary{list-style:none;display:grid;grid-template-columns:82px minmax(0,1fr) 58px 18px;align-items:center;gap:13px;padding:20px;cursor:pointer}.capability summary::-webkit-details-marker{display:none}.capability summary:hover{background:#292832}.cap-id{font:11px var(--mono);color:var(--accent)}.cap-name{font-size:15px;font-weight:500;line-height:1.45}.cap-version{font:11px var(--mono);color:var(--muted)}.disclosure{font-size:20px;line-height:1;text-align:center;color:var(--muted)}.capability[open] .disclosure{font-size:0}.capability[open] .disclosure:after{content:"−";font-size:20px}.cap-body{border-top:1px solid var(--line);padding:24px 28px;font-size:14px;color:var(--muted)}.cap-body strong{color:var(--text)}.cap-body>:last-child{margin-bottom:0}.capability:target{outline:2px solid var(--accent);outline-offset:3px}.empty{padding:30px;border:1px dashed var(--line);text-align:center;color:var(--muted)}.table-scroll{max-width:100%;overflow:auto;border:1px solid var(--line);border-radius:8px;margin:25px 0}table{width:100%;border-collapse:collapse;font-size:13px;min-width:660px}th{text-align:left;font-size:11px;color:var(--muted);font-weight:500;text-transform:uppercase;letter-spacing:.05em;background:var(--surface)}th,td{padding:16px;vertical-align:top;border-bottom:1px solid var(--line)}tr:last-child td{border-bottom:0}td:first-child{font:11px/1.7 var(--mono);color:var(--accent)}.evidence-copy,.prose{font-size:14px;color:var(--muted)}.prose strong{color:var(--text)}.prose h3{font-size:19px}.source-intro{border-bottom:1px solid var(--line);padding-bottom:15px;margin-bottom:25px}.source-intro p:first-child{color:var(--accent);font-size:12px}.footer{padding:30px 0 35px;border-top:1px solid var(--line);display:flex;justify-content:space-between;gap:20px;font-size:12px;color:var(--muted)}.footer a{white-space:nowrap}.support-grid{display:grid;grid-template-columns:1fr 1fr;gap:36px}.support-grid h2{font-size:25px}.no-js{font-size:13px;color:var(--gold)}
@media(min-width:1500px){.main{padding-right:10%}}@media(max-width:1000px){.layout{grid-template-columns:180px minmax(0,1fr)}.sidebar{padding:30px 15px}.main{padding:0 5%}.milestone{grid-template-columns:112px 1fr}.capability summary{grid-template-columns:73px minmax(0,1fr) 17px}.cap-version{display:none}.support-grid{grid-template-columns:1fr;gap:10px}}
@media(max-width:700px){.layout{display:block}.sidebar{position:static;height:auto;padding:18px 6%;border-right:0;border-bottom:1px solid var(--line);gap:18px}.brand{font-size:22px}.brand svg{width:34px;height:34px}.sidebar nav{flex-direction:row;overflow:auto;gap:10px}.sidebar nav a{white-space:nowrap;font-size:12px;padding:5px 0;border:0}.sidebar-note{display:none}.main{padding:0 6%}.topline{padding:18px 0;font-size:10px;gap:12px}.topline a{font-size:12px}.hero{padding:42px 0 28px}h1{font-size:48px}.hero-lead{font-size:17px}.proof{grid-template-columns:1fr;margin-bottom:36px}.proof-label{padding:12px 18px;border-right:0;border-bottom:1px solid #3d6252;flex-direction:row;gap:12px;justify-content:flex-start;font-size:11px}.proof-main{padding:20px 18px}.proof-main h2{font-size:19px}.section-top{display:block}.section-top small{display:block;margin-bottom:20px}h2{font-size:27px}.milestone{grid-template-columns:92px 1fr;padding:24px 0}.version>span{font-size:21px}.milestone-main h3{font-size:20px}.outcome{font-size:14px}.capability summary{padding:16px 13px;gap:9px;grid-template-columns:67px minmax(0,1fr) 15px}.cap-name{font-size:14px}.cap-id{font-size:10px}.cap-body{padding:19px 17px}.toolbar-actions{width:100%}.field-select{flex:1}.section-top h2{margin-bottom:12px}.footer{flex-direction:column;gap:8px}.block{padding:25px 0 32px}}
@media(prefers-reduced-motion:reduce){html{scroll-behavior:auto}}
@media print{:root{color-scheme:light;--page:#fff;--surface:#f4f4f7;--raised:#eee;--line:#ccc;--text:#181820;--muted:#454550;--accent:#4742a8;--green:#19643e}body{font-size:10pt}.layout{display:block}.sidebar,.toolbar,.results,.topline,.hero-bottom button,.skip,.no-js{display:none!important}.main{padding:0}.hero{padding:20px 0}h1{font-size:36pt}.proof{background:#f1f8f3;border-color:#8a9}.proof-label,.proof-main p{color:#234f36}.proof-label{border-color:#8a9}.proof-main a{color:#19643e}.milestone{break-inside:avoid}.capability{break-inside:avoid}.capability summary{padding:12px}.cap-body{padding:15px}.table-scroll{overflow:visible}table{min-width:0}.support-grid{display:block}.block{padding:20px 0}.footer{padding:16px 0}.id-link{color:#4742a8}a{color:#4742a8;text-decoration:none}.cap-version{display:block}.capability summary{grid-template-columns:75px 1fr 55px 15px}}
'''

css += r'''
.sidebar{gap:25px}.sidebar nav a{padding:8px 11px}.hero h1{font-size:clamp(48px,5.4vw,72px)}.hero-lead{max-width:780px}.hero-bottom .text-action{font-size:13px;margin-left:3px}.platform-map{margin:32px 0 42px;padding:26px;border:1px solid var(--line);border-radius:10px;background:var(--surface)}.map-caption{font:11px var(--mono);color:var(--muted);margin-bottom:22px}.app-row,.role-row{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:12px}.map-node{border:1px solid var(--line);border-radius:6px;padding:16px;font-size:14px;text-align:center}.map-node small{display:block;font-size:11px;color:var(--muted);margin-top:4px}.map-core{margin:20px 0;padding:20px;text-align:center;background:#302d55;border:1px solid #6e6af0;border-radius:7px}.map-core strong{display:block;font-size:19px}.map-core span{font-size:12px;color:#d0cdef}.role-row .map-node{font-size:12px;padding:12px 6px}.map-foot{margin:18px 0 0;color:var(--muted);font-size:12px;text-align:center}.stream-label{display:block;font:10px var(--mono);color:var(--accent);text-transform:uppercase;letter-spacing:.08em;margin-bottom:16px}.prose blockquote{border-left:3px solid var(--purple);padding:10px 20px;margin:22px 0;background:var(--surface);color:var(--text);font-size:17px}.prose blockquote p:last-child{margin-bottom:0}.prose h3{margin-top:35px;margin-bottom:18px}.prose>h3:first-child{margin-top:0}.prose .table-scroll{margin-top:22px;margin-bottom:28px}.prose p,.prose li{max-width:880px}.prose .table-scroll p{margin-bottom:0}.cap-body a{overflow-wrap:anywhere}.field-stream{flex:0 1 205px}.main>.block{border-top:1px solid var(--line)}.reading-note{font-size:13px;color:var(--muted);border-left:2px solid var(--purple);padding-left:14px}.milestone-main .id-links{gap:6px}.proof{margin-bottom:35px}.source-intro{border-bottom:0}.strategy-section h2{max-width:800px}.strategy-section{padding-top:42px;padding-bottom:38px}
@media(max-width:700px){.hero h1{font-size:48px}.platform-map{padding:18px 14px}.app-row{grid-template-columns:1fr;gap:8px}.app-row .map-node{padding:10px}.app-row .map-node small{display:inline;margin-left:6px}.role-row{grid-template-columns:1fr;gap:8px}.role-row .map-node{padding:8px}.map-core{margin:14px 0}.field-stream{flex:1 1 100%}.toolbar-actions{margin-top:2px}.strategy-section h2{font-size:26px}.prose blockquote{font-size:15px;padding:12px 15px}.sidebar nav{gap:18px}.sidebar nav a{padding:4px 0}.hero-bottom{gap:14px}.hero-bottom .text-action{width:100%;margin-top:8px}}
@media print{.platform-map{break-inside:avoid;background:#fff}.map-core{background:#eeedff;color:#181820;border-color:#777}.map-core span{color:#454550}.app-row{grid-template-columns:repeat(3,1fr)}.role-row{grid-template-columns:repeat(3,1fr)}.prose blockquote{font-size:11pt}.hero h1{font-size:34pt}.strategy-section{padding:18px 0}}
'''

css += r'''
.live-panel{margin:24px 0 10px;border:1px solid var(--purple);border-left:5px solid var(--accent);border-radius:9px;padding:25px;background:#252238;scroll-margin-top:20px}.live-heading{display:flex;justify-content:space-between;gap:15px;flex-wrap:wrap;font-size:11px;color:var(--muted);margin-bottom:17px}.live-heading strong{color:var(--accent);font:12px var(--mono);letter-spacing:.06em}.live-panel h2{font-size:27px;line-height:1.25;margin-bottom:12px;max-width:780px}.live-panel .next-link{font:12px var(--mono)}.live-reason{font-size:14px;color:#d0cbe6;margin:10px 0 16px;max-width:820px}.next-checks{font-size:13px;color:var(--muted);padding-left:21px;margin-bottom:20px}.next-checks li{margin-bottom:6px}.live-totals{display:flex;flex-wrap:wrap;gap:8px;margin:20px 0 9px}.live-totals span{border:1px solid #514b6a;border-radius:5px;font-size:11px;padding:5px 9px}.live-footnote{font-size:11px;color:var(--muted);margin-bottom:0}.live-feed{margin-top:19px;border-top:1px solid #514b6a;padding-top:15px;font-size:12px;color:var(--muted)}.live-feed summary{cursor:pointer;color:var(--text)}.live-feed ul{padding-left:18px;margin-bottom:0}.live-feed time{font:10px var(--mono);color:var(--accent);margin-right:8px}.live-sessions{font-size:12px;color:var(--muted);margin-top:14px}.capability.is-next{border-color:var(--accent);box-shadow:inset 4px 0 0 var(--accent)}.capability.is-next .cap-id:after{content:'NEXT';display:block;font:9px var(--mono);color:var(--text);margin-top:7px}.cap-work-status{display:block;font:10px/1.5 var(--mono);color:var(--muted);margin-top:5px}.capability[data-state=done] .cap-work-status{color:var(--green)}.capability[data-state=blocked] .cap-work-status{color:var(--gold)}.capability[data-state=in_progress] .cap-work-status{color:var(--accent)}.work-record{background:var(--page);padding:14px;border:1px solid var(--line);border-radius:5px;margin-bottom:22px}.work-record p{margin:6px 0}.work-record ul:empty{display:none}.connection-ok{color:var(--green)}.connection-off{color:var(--gold)}.hero{padding-top:36px}.field-state{flex:0 1 155px}
@media(max-width:700px){.live-panel{padding:19px 15px}.live-panel h2{font-size:22px}.live-heading{gap:9px}.field-state{flex:1}.live-heading span{font-size:10px}}@media print{.live-panel{background:#f3f1ff;break-inside:avoid}.live-reason{color:var(--muted)}.live-feed{display:none}.work-record{background:#f7f7f7}}
'''

js = r'''
const cards = [...document.querySelectorAll('.capability')];
const search = document.querySelector('#search');
const version = document.querySelector('#version');
const stream = document.querySelector('#stream');
const workFilter = document.querySelector('#work-state');
const count = document.querySelector('#count');
const empty = document.querySelector('#empty');
const expand = document.querySelector('#expand');
let expanded = false;
const searchable = new Map(cards.map(card => [card, card.textContent.toLowerCase()]));
function filter() {
  const words = search.value.toLowerCase().trim().split(/\s+/).filter(Boolean);
  let visible = 0;
  cards.forEach(card => {
    card.hidden = !(words.every(word => searchable.get(card).includes(word)) && (!version.value || card.dataset.version === version.value) && (!stream.value || card.dataset.stream === stream.value) && (!workFilter.value || card.dataset.state === workFilter.value));
    if (!card.hidden) visible++;
  });
  count.textContent = `${visible} of ${cards.length} capabilities`;
  empty.hidden = visible !== 0;
}
search.addEventListener('input', filter);
version.addEventListener('change', filter);
stream.addEventListener('change', filter);
workFilter.addEventListener('change', filter);
document.querySelector('#reset').addEventListener('click', () => { search.value = ''; version.value = ''; stream.value = ''; workFilter.value = ''; filter(); });
expand.addEventListener('click', () => {
  expanded = !expanded;
  cards.filter(card => !card.hidden).forEach(card => { card.open = expanded; });
  expand.textContent = expanded ? 'Collapse details' : 'Expand details';
  expand.setAttribute('aria-expanded', String(expanded));
});
function revealHash() {
  const card = document.getElementById(decodeURIComponent(location.hash.slice(1)));
  if (card?.classList.contains('capability')) {
    search.value = ''; version.value = ''; stream.value = ''; workFilter.value = ''; filter(); card.open = true;
    requestAnimationFrame(() => card.scrollIntoView({block: 'start'}));
  }
}
window.addEventListener('hashchange', revealHash);
document.querySelectorAll('a[href^="#"]').forEach(link => link.addEventListener('click', () => {
  if (location.hash === link.hash) revealHash();
}));
let printState;
window.addEventListener('beforeprint', () => {
  printState = cards.map(card => ({open: card.open, hidden: card.hidden}));
  cards.forEach(card => { card.open = true; card.hidden = false; });
});
window.addEventListener('afterprint', () => {
  if (printState) cards.forEach((card, i) => Object.assign(card, printState[i]));
});
document.querySelector('#print').addEventListener('click', () => window.print());
const nav = [...document.querySelectorAll('.sidebar nav a')];
if ('IntersectionObserver' in window) {
  const observer = new IntersectionObserver(entries => {
    const active = entries.find(entry => entry.isIntersecting);
    if (active) nav.forEach(link => {
      if (link.hash === '#' + active.target.id) link.setAttribute('aria-current', 'location');
      else link.removeAttribute('aria-current');
    });
  }, {rootMargin: '-10% 0px -65% 0px'});
  nav.forEach(link => observer.observe(document.querySelector(link.hash)));
}
filter(); revealHash();
document.querySelectorAll('[data-js]').forEach(el => { el.hidden = false; });

const initial = JSON.parse(document.querySelector('#roadmap-state').textContent);
const labels = {planned: 'Planned gate', in_progress: 'In progress', blocked: 'Blocked', done: 'Complete'};
const formatDate = value => new Intl.DateTimeFormat('en-GB', {timeZone: 'Europe/London', day:'2-digit', month:'short', hour:'2-digit', minute:'2-digit', second:'2-digit'}).format(new Date(value));
function listItems(element, values) {
  element.replaceChildren(...values.map(value => { const li = document.createElement('li'); li.textContent = value; return li; }));
}
function applyState(state) {
  const step = state.next_step;
  document.querySelector('#next-title').textContent = step?.title || 'Review blockers and select the next supported step';
  const link = document.querySelector('#next-capability');
  link.textContent = step?.capability || 'View capability gates';
  link.href = step ? '#' + step.capability : '#capabilities';
  document.querySelector('#next-reason').textContent = step?.why || 'No eligible core capability is currently selected.';
  listItems(document.querySelector('#next-checks'), step?.checks || []);
  document.querySelector('#state-updated').textContent = 'Work state updated ' + formatDate(state.updated_at);
  const totals = {planned:0, in_progress:0, blocked:0, done:0};
  cards.forEach(card => {
    const item = state.capabilities[card.id];
    card.dataset.state = item.status;
    card.classList.toggle('is-next', step?.capability === card.id);
    card.querySelector('.cap-work-status').textContent = labels[item.status];
    card.querySelector('.work-note').textContent = item.note;
    listItems(card.querySelector('.work-evidence'), item.evidence);
    totals[item.status]++;
  });
  for (const [key,value] of Object.entries(totals)) document.querySelector('[data-total="'+key+'"]').textContent = value;
  const sessions = Object.values(state.sessions).sort((a,b)=>b.updated_at.localeCompare(a.updated_at)).slice(0,5);
  listItems(document.querySelector('#session-list'), sessions.map(item => `${item.title} — ${item.status} (last reported ${formatDate(item.updated_at)}). ${item.note}`));
  listItems(document.querySelector('#activity-list'), state.activity.slice(0,8).map(item => `${formatDate(item.at)} · ${item.note}`));
  filter();
}
applyState(initial);
let polling = false;
async function sync() {
  if (polling || document.hidden) return;
  polling = true;
  const badge = document.querySelector('#live-connection');
  try {
    const response = await fetch('/api/state', {cache:'no-store', signal:AbortSignal.timeout(5000)});
    if (!response.ok) throw Error('Preview unavailable');
    const data = await response.json();
    if (data.service !== 'veil-roadmap') throw Error('Wrong preview service');
    if (data.document_revision !== document.querySelector('meta[name="roadmap-document-revision"]').content) {
      sessionStorage.setItem('veil-roadmap-ui', JSON.stringify({search:search.value,version:version.value,stream:stream.value,status:workFilter.value,open:cards.filter(c=>c.open).map(c=>c.id),scroll:scrollY}));
      location.reload(); return;
    }
    applyState(data.state);
    badge.textContent = 'Live · synced ' + formatDate(new Date().toISOString());
    badge.className = 'connection-ok';
  } catch {
    badge.textContent = 'Live preview disconnected · showing last saved state';
    badge.className = 'connection-off';
  } finally { polling = false; }
}
const saved = sessionStorage.getItem('veil-roadmap-ui');
if (saved) {
  try {
    const ui = JSON.parse(saved);search.value=ui.search;version.value=ui.version;stream.value=ui.stream;workFilter.value=ui.status;
    cards.forEach(c=>{c.open=ui.open.includes(c.id);});filter();requestAnimationFrame(()=>scrollTo(0,ui.scroll));
  } finally { sessionStorage.removeItem('veil-roadmap-ui'); }
}
if (location.protocol === 'http:' && ['127.0.0.1','localhost'].includes(location.hostname)) {
  sync();setInterval(sync,2000);document.addEventListener('visibilitychange',()=>{if(!document.hidden)sync();});
}
'''

def section(title, anchor, display=None):
    return f'<section id="{anchor}" class="block strategy-section"><h2>{escape(display or title)}</h2><div class="prose">{render(sections[title])}</div></section>'

nav_items = [('live', 'Now and next'), ('mission', 'Mission and decisions'), ('platform', 'App integrations'),
             ('priorities', 'Delivery sequence'), ('milestones', 'Version milestones'),
             ('capabilities', 'Capability explorer'), ('ownership', 'Trust and data'),
             ('validation', 'Evidence and testing'), ('gates', 'Release gates'), ('next', 'Next work slices')]
navigation = ''.join(f'<a href="#{anchor}">{label}</a>' for anchor, label in nav_items)
all_versions = sorted(set(versions.values()), key=lambda v: tuple(map(int, v.split('.'))) if v != 'Unassigned' else (999,))
version_options = ''.join(f'<option value="{v}">{"Future / unassigned" if v == "Unassigned" else v}</option>' for v in all_versions)
stream_options = ''.join(f'<option>{escape(stream)}</option>' for stream in sorted(set(streams.values())))
count_caps = len(cap_html)
totals = {status: sum(item['status'] == status for item in state['capabilities'].values()) for status in state_labels}
next_step = state.get('next_step')
next_title = next_step['title'] if next_step else 'Review blockers and select the next supported step'
next_id = next_step['capability'] if next_step else 'capabilities'
next_checks = ''.join('<li>' + escape(x) + '</li>' for x in next_step['checks']) if next_step else ''
serialized_state = json.dumps(state, ensure_ascii=False).replace('<', '\\u003c')
html = f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="description" content="Veil’s Modular, Agnostic, Shared calling platform: local-first private calls, an embeddable engine/SDK, chosen infrastructure, version milestones and evidence gates.">
<meta name="theme-color" content="#17161c"><meta name="roadmap-document-revision" content="{document_revision}"><title>Veil — Shared calling platform roadmap</title><style>{css}</style></head>
<body><a class="skip" href="#main">Skip to roadmap</a><div class="layout">
<aside class="sidebar"><a class="brand" href="#main">{logo}<span>Veil</span></a>
<nav aria-label="Roadmap sections">{navigation}</nav>
<div class="sidebar-note"><b>Your app. Your devices.<br>Your infrastructure.</b><p>One shared call engine.<br>Local first. Peer to peer.<br>No company contact database.</p><a href="ROADMAP.md">Markdown source ↗</a></div></aside>
<main id="main" class="main"><div class="topline"><span>Platform / Product roadmap</span><span>23 November 2026</span></div>
<section id="live" class="live-panel" aria-labelledby="next-title"><div class="live-heading"><strong>Next step</strong><span id="live-connection">Saved snapshot · <a href="{escape(live_url)}/#live">open live roadmap</a></span></div>
<h2 id="next-title">{escape(next_title)}</h2><a id="next-capability" class="next-link" href="#{next_id}">{next_id}</a>
<p id="next-reason" class="live-reason">{escape(next_step['why'] if next_step else '')}</p><ol id="next-checks" class="next-checks">{next_checks}</ol>
<div class="live-totals"><span><b data-total="in_progress">{totals['in_progress']}</b> in progress</span><span><b data-total="blocked">{totals['blocked']}</b> blocked</span><span><b data-total="done">{totals['done']}</b> complete</span><span><b data-total="planned">{totals['planned']}</b> planned gates</span></div>
<p class="live-footnote"><span id="state-updated">{escape(state['updated_at'])}</span>. Counts refer to full capability gates, not percentage of code implemented.</p>
<details class="live-feed"><summary>Reported session activity and recent changes</summary><div class="live-sessions"><strong>Sessions</strong><ul id="session-list"></ul></div><ul id="activity-list"></ul></details></section>
<header class="hero"><span class="eyebrow">Veil · Video and audio, under your control</span><h1>Modular.<br>Agnostic.<br>Shared.</h1>
<p class="hero-lead">A <strong>calling app and embeddable call engine</strong>. Peer to peer by default. Useful on a surviving local network when internet fails. Your identities, your chosen helpers, the same protocol inside other apps.</p>
<div class="hero-bottom"><span class="chip live">Desktop 0.1.13 released</span><span class="chip">Android development preview</span><button id="print" data-js hidden type="button">Print / save PDF</button><a class="text-action" href="#platform">Explore the provider architecture ↓</a></div></header>
<figure class="platform-map" aria-label="Target architecture: standalone Veil, a reference host such as Mailcraft and other apps share the Veil engine, which uses local networks, direct peer paths or chosen helpers.">
<figcaption class="map-caption">TARGET ARCHITECTURE · SDK and provider surface planned</figcaption>
<div class="app-row"><div class="map-node">Standalone Veil<small>First-party client</small></div><div class="map-node">Reference host<small>e.g. Mailcraft</small></div><div class="map-node">Other apps / plugins<small>Platform-dependent adapters</small></div></div>
<div class="map-core"><strong>One Veil engine + provider contract</strong><span>Identity · admission · membership · media · recovery · policy</span></div>
<div class="role-row"><div class="map-node">Local network / hotspot</div><div class="map-node">Direct peer connection</div><div class="map-node">Chosen replaceable helpers</div></div>
<p class="map-foot">A shared protocol. No shared company address book. Helpers receive no conversation keys.</p></figure>
<div class="proof"><div class="proof-label"><span>TEST-5G-01</span><span>Connection passed</span></div><div class="proof-main"><h2>Phone on 5G → desktop host</h2><p>Successfully tested by Jeremy; reported 23 November. Route, duration and detailed media checks remain unspecified. <a href="#evidence">View evidence</a></p></div></div>
{section('Mission and session decisions', 'mission')}
{section('Embeddable call platform', 'platform')}
{section('Priority and delivery approach', 'priorities')}
<section id="milestones" class="block"><div class="section-top"><h2>Version milestones</h2><small>Proposed sequence · no fixed dates</small></div>
<p class="section-lead">Each version earns a concrete user outcome. Capability IDs connect the release to the work and evidence needed to complete it.</p>
<div class="milestones">{''.join(milestones)}</div><div class="notes">{render(milestone_notes)}</div></section>
<section id="capabilities" class="block"><div class="section-top"><h2>Capability explorer</h2><small>{count_caps} capabilities · stable IDs</small></div>
<p class="section-lead">Outcomes, architecture boundaries, dependencies and completion criteria across the app, SDK and infrastructure. Open an item or follow a dependency. Future extensions have design gates rather than invented release dates.</p>
<noscript><p class="no-js">All capability details are available below. Open each heading to read them.</p></noscript>
<div class="toolbar" data-js hidden><div class="field"><label for="search">Search capabilities</label><input id="search" type="search" placeholder="Try recovery, audio or TRUST-01" autocomplete="off"></div>
<div class="field field-select"><label for="version">Target version</label><select id="version"><option value="">All versions</option>{version_options}</select></div>
<div class="field field-stream"><label for="stream">Workstream</label><select id="stream"><option value="">All workstreams</option>{stream_options}</select></div>
<div class="field field-state"><label for="work-state">Work state</label><select id="work-state"><option value="">All states</option><option value="planned">Planned gate</option><option value="in_progress">In progress</option><option value="blocked">Blocked</option><option value="done">Complete</option></select></div>
<div class="toolbar-actions"><button id="expand" type="button" aria-expanded="false">Expand details</button></div></div>
<div class="results" data-js hidden><span id="count" role="status" aria-live="polite">{count_caps} of {count_caps} capabilities</span><button id="reset" type="button">Clear filters</button></div>
<div id="capability-list">{''.join(cap_html)}</div><p id="empty" class="empty" hidden>No capabilities match. Clear the filters or try a different term.</p></section>
{section('Data ownership and trust boundaries', 'ownership')}
{section('System roles and extension boundaries', 'roles')}
{section('Product journeys and failure states', 'journeys')}
{section('What the mission does and does not promise', 'boundaries')}
{section('Current evidence', 'evidence', 'What has already been demonstrated')}
{section('Validation scenarios and evidence', 'validation')}
{section('Release gates and deliverables', 'gates')}
{section('Open decisions and engineering risks', 'decisions')}
{section('Next implementation slices', 'next')}
<section id="principles" class="block"><h2>Principles and product definition</h2><div class="prose source-intro">{render(intro)}</div><div class="prose">{render(sections['Principles'])}</div></section>
{section('Version and compatibility rules', 'compatibility')}
{section('Work after the core milestones', 'extensions')}
{section('Maintaining the roadmap', 'maintenance')}
<footer class="footer"><span>Veil · Modular, agnostic, shared.</span><span>Planning targets · <a href="ROADMAP.md">Read the Markdown source ↗</a></span></footer>
</main></div><script type="application/json" id="roadmap-state">{serialized_state}</script><script>{js}</script></body></html>'''
atomic_write(OUTPUT, html)
print(f'Created {OUTPUT} ({len(html.encode("utf-8")):,} bytes), with {len(cap_html)} capabilities and {len(milestones)} milestones.')
