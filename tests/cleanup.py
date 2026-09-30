#!/usr/bin/env python3
"""Exercise recovery ownership checks against a deterministic CLI fixture."""
import json, os, subprocess, sys, tempfile
from pathlib import Path
binary = Path(sys.argv[1]).resolve()
base = 'io.github.tcballard.widget-core-bencher.123-4.'
with tempfile.TemporaryDirectory(prefix='bencher recovery $literal ') as tmp:
    root = Path(tmp)
    cli = root / 'core fixture'
    log = root / 'calls.jsonl'
    cli.write_text('''#!/usr/bin/env python3
import json, os, sys
args=sys.argv[1:]
with open(os.environ['BENCH_TEST_LOG'],'a') as out:out.write(json.dumps(args)+'\\n')
if args==['list']:
    print(json.dumps({'catalog':[{'packageId':i} for i in json.loads(os.environ['BENCH_TEST_PACKAGES'])]}))
elif args[0]=='uninstall':
    if os.environ.get('BENCH_TEST_FAIL')=='1':
        print(json.dumps({'error':'fixture uninstall failed'}));sys.exit(1)
    print('{}')
else:sys.exit(2)
''')
    cli.chmod(0o755)
    env = dict(os.environ, BENCH_TEST_LOG=str(log), BENCH_TEST_PACKAGES=json.dumps([base+'target',base+'witness','io.example.user-clock']))
    def run(packages, fail=False):
        (root / 'ownership.json').write_text(json.dumps({'schemaVersion':1,'runId':'123-4','packages':packages}))
        log.unlink(missing_ok=True)
        result = subprocess.run([str(binary),'cleanup','--output',str(root),'--core',str(cli)], env=dict(env, BENCH_TEST_FAIL='1' if fail else '0'),capture_output=True,text=True,timeout=10)
        calls = [json.loads(s) for s in log.read_text().splitlines()] if log.exists() else []
        return result,calls
    result,calls = run(['io.example.user-clock'])
    assert result.returncode != 0 and calls == [], (result,calls)
    result,calls = run([base+'target',base+'witness'])
    assert result.returncode == 0
    assert calls == [['list'],['uninstall',base+'witness','delete'],['uninstall',base+'target','delete']], calls
    result,calls = run([base+'target',base+'witness'], fail=True)
    assert result.returncode != 0 and len(calls) == 3
    assert 'fixture uninstall failed' in result.stderr
    env['BENCH_TEST_PACKAGES'] = '[]'
    result,calls = run([base+'target',base+'witness'])
    assert result.returncode == 0 and calls == [['list']]
print('PASS: ownership refusal, exact cleanup, attempt-all on error and idempotent recovery')
