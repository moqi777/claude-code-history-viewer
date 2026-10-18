"""One-shot Windows acceptance after midnight and after an actual reboot/login.

prepare <installed AgentKB.exe> creates isolated fixtures under TEMP, a fake AI,
and two current-user tasks. No real records/AI are used. Tasks delete themselves
after recording their result; unused tasks expire after seven days. Results and
fixtures remain in the printed temporary folder for review. Never reboots/logs out.
"""
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid
from xml.sax.saxutils import escape


def command(args, **kwargs):
    return subprocess.run(args, capture_output=True, timeout=90,
                          creationflags=subprocess.CREATE_NO_WINDOW, **kwargs)


def system_status():
    script = """$boot=(Get-CimInstance Win32_OperatingSystem).LastBootUpTime;
$t=Get-ScheduledTask -TaskName io.github.moqi777.agentkb.sync;
$i=$t|Get-ScheduledTaskInfo;
[pscustomobject]@{boot=$boot.ToUniversalTime().ToString('o');
last_run=$i.LastRunTime.ToUniversalTime().ToString('o');result=$i.LastTaskResult;
state=$t.State.ToString();execute=$t.Actions.Execute;arguments=$t.Actions.Arguments}|ConvertTo-Json -Compress"""
    result = command(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script])
    assert result.returncode == 0, result.stderr.decode(errors='replace')
    return json.loads(result.stdout)


def environment(home, fake):
    return {**os.environ, 'CCHV_TEST_HOME': str(home), 'AGENTKB_DATA_DIR': str(home/'AgentKB'),
            'USERPROFILE': str(home), 'APPDATA': str(home/'AppData'),
            'LOCALAPPDATA': str(home/'Local AppData'), 'TEMP': str(home), 'TMP': str(home),
            'FAKE_AI_DIR': str(fake)}


def sync(exe, home, fake, scheduled=True):
    args = [str(exe), '--kb-sync'] + (['--scheduled'] if scheduled else [])
    result = command(args, env=environment(home, fake))
    assert result.returncode == 0, result.stderr.decode(errors='replace')
    data = json.loads(result.stdout)
    assert data['status'] == 'ok', data
    return data


def counts(home):
    conn = sqlite3.connect(home/'AgentKB/kb.db')
    try:
        return {'notes': conn.execute('SELECT count(*) FROM notes').fetchone()[0],
                'rounds': [list(row) for row in conn.execute('SELECT round_id,state FROM digest_rounds ORDER BY round_id')]}
    finally:
        conn.close()


def wait_notes(home, expected):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        result = counts(home)
        if result['notes'] == expected and all(row[1] == 'done' for row in result['rounds']):
            return result
        time.sleep(0.5)
    raise RuntimeError(f'expected {expected} notes: {result}')


def sources(home):
    return {str(p.relative_to(home)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (home/'.claude').rglob('*.jsonl')}


def session(home, number, timestamp):
    sid = f'11111111-2222-3333-4444-{number:012}'
    path = home/f'.claude/projects/fixture/{sid}.jsonl'
    path.parent.mkdir(parents=True, exist_ok=True)
    row = {'type': 'user', 'uuid': f'u{number}', 'sessionId': sid, 'timestamp': timestamp,
           'cwd': str(home/'中文项目'), 'message': {'role': 'user', 'content': f'隔离验收会话 {number} 数据库测试'}}
    path.write_text(json.dumps(row, ensure_ascii=False, separators=(',', ':'))+'\n', encoding='utf-8')


def fixture(home, fake, digest):
    home.mkdir()
    config = home/'.config/agent-kb/config.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'version': 1, 'data_dir': str(home/'AgentKB'), 'schedule_enabled': False,
        'sources': {'claude': {'enabled': True, 'root': str(home/'.claude')},
                    'codex': {'enabled': False, 'root': str(home/'.codex')}},
        'semantic': {'enabled': False}, 'digest': {'enabled': digest, 'engine': 'claude',
            'cli_path': str(fake/'fake-ai.exe'), 'time': '00:00', 'quiet_minutes': 0,
            'min_messages': 1, 'recent_days': 365, 'max_sessions_per_round': 1}}, ensure_ascii=False), encoding='utf-8')


def prepare(exe):
    exe = Path(exe).resolve(strict=True)
    assert exe.name.lower() == 'agentkb.exe' and exe.with_name('agentkb-cli.exe').is_file()
    root = Path(tempfile.mkdtemp(prefix='agentkb-p7-deferred-'))
    fake = root/'fake-ai'
    fake.mkdir()
    repository = Path(__file__).resolve().parent.parent
    built = command(['rustc', str(repository/'src-tauri/tests/fake_ai/fake-ai.rs'), '-o', str(fake/'fake-ai.exe')])
    assert built.returncode == 0, built.stderr.decode(errors='replace')
    (fake/'mode').write_text('ok', encoding='utf-8')
    now = datetime.now().astimezone()
    midnight = (now + timedelta(days=1)).replace(hour=0, minute=1, second=0, microsecond=0)
    expiry = now + timedelta(days=7)
    for kind in ['midnight', 'logon']:
        home = root/kind
        fixture(home, fake, kind == 'midnight')
        session(home, 1, (now-timedelta(days=1)).astimezone(timezone.utc).isoformat().replace('+00:00', 'Z'))
    sync(exe, root/'midnight', fake)
    baseline = wait_notes(root/'midnight', 1)
    assert baseline['rounds'] == [[now.date().isoformat(), 'done']], baseline
    # After today's cutoff; only the next day's automatic round may digest it.
    session(root/'midnight', 2, now.astimezone(timezone.utc).isoformat().replace('+00:00', 'Z'))
    sync(exe, root/'midnight', fake, scheduled=False)
    identifier = uuid.uuid4().hex
    state = {'id': identifier, 'exe': str(exe), 'created': now.isoformat(),
             'due': midnight.isoformat(), 'expiry': expiry.isoformat(),
             'system': system_status(), 'baseline': baseline,
             'sources': {kind: sources(root/kind) for kind in ['midnight', 'logon']},
             'tasks': {kind: f'AgentKB-P7-deferred-{identifier}-{kind}' for kind in ['midnight', 'logon']}}
    (root/'state.json').write_text(json.dumps(state, ensure_ascii=False, indent=2), encoding='utf-8')
    shutil.copy2(__file__, root/'runner.py')
    user = command(['whoami.exe']).stdout.decode().strip()
    pythonw = Path(sys.executable).with_name('pythonw.exe')
    assert pythonw.is_file()
    registered = []
    try:
        for kind, name in state['tasks'].items():
            end = f'<EndBoundary>{expiry.isoformat(timespec="seconds")}</EndBoundary>'
            trigger = (f'<TimeTrigger><StartBoundary>{midnight.isoformat(timespec="seconds")}</StartBoundary>{end}<Enabled>true</Enabled></TimeTrigger>'
                       if kind == 'midnight' else f'<LogonTrigger>{end}<Enabled>true</Enabled><UserId>{escape(user)}</UserId><Delay>PT2M</Delay></LogonTrigger>')
            args = subprocess.list2cmdline([str(root/'runner.py'), 'run', str(root), kind])
            xml = f'''<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
<RegistrationInfo><Description>AgentKB isolated P7 {kind} acceptance; fake data only; expires in 7 days</Description></RegistrationInfo>
<Triggers>{trigger}</Triggers>
<Principals><Principal id="Author"><UserId>{escape(user)}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><DeleteExpiredTaskAfter>PT1H</DeleteExpiredTaskAfter><ExecutionTimeLimit>PT5M</ExecutionTimeLimit></Settings>
<Actions Context="Author"><Exec><Command>{escape(str(pythonw))}</Command><Arguments>{escape(args)}</Arguments><WorkingDirectory>{escape(str(root))}</WorkingDirectory></Exec></Actions></Task>'''
            definition = root/f'{kind}.xml'
            definition.write_text(xml, encoding='utf-16')
            result = command(['schtasks.exe', '/Create', '/TN', name, '/XML', str(definition)])
            assert result.returncode == 0, result.stderr.decode(errors='replace')
            registered.append(name)
    except BaseException:
        for name in registered:
            command(['schtasks.exe', '/Delete', '/TN', name, '/F'])
        raise
    print(json.dumps({'root': str(root), **state}, ensure_ascii=False, indent=2))


def run(root, kind):
    root = Path(root).resolve(strict=True)
    assert root.name.startswith('agentkb-p7-deferred-') and kind in ['midnight', 'logon']
    state = json.loads((root/'state.json').read_text(encoding='utf-8'))
    assert len(state['id']) == 32 and all(c in '0123456789abcdef' for c in state['id'])
    task = f"AgentKB-P7-deferred-{state['id']}-{kind}"
    assert state['tasks'][kind] == task
    result = {'kind': kind, 'at': datetime.now().astimezone().isoformat(), 'status': 'pending'}
    finished = False
    try:
        system = system_status()
        result['system'] = system
        if kind == 'logon' and system['boot'] == state['system']['boot']:
            result['reason'] = 'No new boot detected; keep waiting for actual reboot/login'
            return
        if kind == 'midnight' and datetime.now().astimezone() < datetime.fromisoformat(state['due']):
            result['reason'] = 'Not yet next-day cutoff; no test executed'
            return
        finished = True
        home = root/kind
        result['sync'] = sync(state['exe'], home, root/'fake-ai')
        if kind == 'midnight':
            result['digest'] = wait_notes(home, 2)
            assert len(result['digest']['rounds']) == 2
            assert result['digest']['rounds'][1][0] > state['baseline']['rounds'][0][0]
        else:
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                system = system_status()
                if system['last_run'] >= system['boot'] and system['result'] == 0 and system['state'] == 'Ready':
                    break
                time.sleep(3)
            assert system['last_run'] >= system['boot'] and system['result'] == 0 and system['state'] == 'Ready', system
            assert system['execute'].casefold() == state['exe'].casefold()
            assert system['arguments'] == '--kb-sync --scheduled'
            result['production_task_observation'] = system
        assert sources(home) == state['sources'][kind], 'Fixture sources changed'
        result['status'] = 'passed'
    except BaseException as error:
        result.update(status='failed', error=repr(error))
    finally:
        if finished:
            try:
                deleted = command(['schtasks.exe', '/Delete', '/TN', task, '/F'])
                result['task_deleted'] = deleted.returncode == 0
            except Exception as error:
                result.update(task_deleted=False, cleanup_error=repr(error))
        (root/f'{kind}-result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    if result['status'] == 'failed':
        raise SystemExit(1)


if __name__ == '__main__':
    if sys.stdout:
        sys.stdout.reconfigure(encoding='utf-8')
    assert os.name == 'nt', 'Windows only'
    if sys.argv[1] == 'prepare':
        prepare(sys.argv[2])
    elif sys.argv[1] == 'run':
        run(sys.argv[2], sys.argv[3])
    else:
        raise SystemExit('usage: prepare <AgentKB.exe> | run <temporary root> midnight|logon')
