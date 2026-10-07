#!/usr/bin/env python3
"""Isolated installation/service-command checks. No real systemctl, output, MIDI or apps."""
import fcntl
import os
from pathlib import Path
import subprocess
import tempfile

binary = Path(__file__).resolve().parents[1] / "target/debug/vibeconsole"
with tempfile.TemporaryDirectory(prefix="vibeconsole-install-") as directory:
    root = Path(directory)
    fake = root / "tools"
    fake.mkdir()
    log = root / "systemctl.log"
    systemctl = fake / "systemctl"
    systemctl.write_text("#!/usr/bin/python3\nimport os,sys\nwith open(os.environ['VIBECONSOLE_CHECK_LOG'],'a') as f: f.write(' '.join(sys.argv[1:])+'\\n')\n")
    systemctl.chmod(0o755)
    env = dict(os.environ, HOME=str(root), XDG_CONFIG_HOME=str(root / "config"),
               XDG_RUNTIME_DIR=str(root), PATH=str(fake) + ":" + os.environ["PATH"],
               VIBECONSOLE_CHECK_LOG=str(log))
    path = root / "config/vibeconsole/mappings.tsv"
    path.parent.mkdir(parents=True)
    saved = b"vibeconsole-mappings-v1\n09e8:1049\tnote\t10\t36\thold\tShift\n"
    path.write_bytes(saved)

    def run(*args, ok=True):
        result = subprocess.run([binary, *args], env=env, capture_output=True, timeout=3)
        assert (result.returncode == 0) == ok, result.stderr
        return result

    installed = root / ".local/bin/vibeconsole"
    unit = root / "config/systemd/user/vibeconsole.service"
    run("install")
    assert installed.is_file() and installed.stat().st_mode & 0o111
    assert not unit.exists() and not log.exists()
    subprocess.run([installed, "--help"], env=env, check=True, stdout=subprocess.DEVNULL)
    staging = installed.with_suffix(".new")
    staging.write_text("occupied by another install")
    run("install", ok=False)
    assert staging.read_text() == "occupied by another install"
    staging.unlink()
    with (root / "vibeconsole.lock").open("r+") as owner:
        fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
        assert b"another VibeConsole session" in run("configure", ok=False).stderr
    run("startup-enable", "--feedback", "--start-flow")
    assert 'daemon --feedback --start-flow' in unit.read_text()
    assert log.read_text().splitlines() == ["--user daemon-reload", "--user enable vibeconsole.service"]
    for cmd in ("startup-start", "startup-status", "startup-stop", "startup-disable"):
        run(cmd)
    managed = unit.read_text()
    verification = root / "vibeconsole.service"
    verification.write_text(managed.replace('%h/.local/bin/vibeconsole', str(binary)))
    subprocess.run(["systemd-analyze", "--user", "verify", verification], check=True, capture_output=True, timeout=5)
    unit.write_text("[Unit]\nDescription=Unmanaged\n")
    run("startup-enable", ok=False)
    run("uninstall", ok=False)
    assert unit.read_text() == "[Unit]\nDescription=Unmanaged\n"
    unit.write_text(managed)
    run("uninstall")
    assert not unit.exists() and not installed.exists()
    assert path.read_bytes() == saved
    assert log.read_text().splitlines()[-2:] == ["--user disable --now vibeconsole.service", "--user daemon-reload"]
    print("installation: PASS (isolated install/remove, opt-in service requests, owner conflict, preserved mappings)")
