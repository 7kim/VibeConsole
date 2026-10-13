#!/usr/bin/env python3
"""Isolated installation/service-command checks. No real systemctl, output, MIDI or apps."""
import fcntl
import os
from pathlib import Path
import subprocess
import signal
import tempfile
import time

binary = Path(__file__).resolve().parents[1] / "target/debug/vibeconsole"
with tempfile.TemporaryDirectory(prefix="vibeconsole-install-") as directory:
    root = Path(directory)
    fake = root / "tools"
    fake.mkdir()
    log = root / "systemctl.log"
    systemctl = fake / "systemctl"
    systemctl.write_text("#!/usr/bin/python3\nimport os,sys\nwith open(os.environ['VIBECONSOLE_CHECK_LOG'],'a') as f: f.write(' '.join(sys.argv[1:])+'\\n')\n")
    systemctl.chmod(0o755)
    lsusb = fake / "lsusb"
    lsusb.write_text("#!/bin/sh\nexit 1\n")
    lsusb.chmod(0o755)
    env = dict(os.environ, HOME=str(root), XDG_CONFIG_HOME=str(root / "config"),
               XDG_RUNTIME_DIR=str(root), PATH=str(fake) + ":" + os.environ["PATH"],
               XDG_DATA_HOME=str(root), XDG_DATA_DIRS=str(root), VIBECONSOLE_CHECK_LOG=str(log))
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
    legacy = run("startup-enable", "--feedback", "--start-flow")
    assert b"Legacy startup-enable flags ignored" in legacy.stdout
    assert 'daemon\n' in unit.read_text()
    assert '--feedback' not in unit.read_text() and '--start-flow' not in unit.read_text()
    daemon = subprocess.Popen([binary, "daemon", "--feedback", "--start-flow"], env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        time.sleep(0.1)
        daemon.send_signal(signal.SIGTERM)
        _, journal = daemon.communicate(timeout=3)
        assert b"Legacy daemon flags ignored" in journal
    finally:
        if daemon.poll() is None:
            daemon.kill()
            daemon.wait(timeout=3)
    assert log.read_text().splitlines() == ["--user daemon-reload", "--user enable vibeconsole.service"]
    for cmd in ("startup-start", "startup-status", "startup-stop", "startup-disable"):
        run(cmd)
    managed = unit.read_text()
    verification = root / "vibeconsole.service"
    verification.write_text(managed.replace('%h/.local/bin/vibeconsole', str(binary)))
    verified = subprocess.run(["systemd-analyze", "--user", "verify", verification], capture_output=True, timeout=5)
    assert verified.returncode == 0, verified.stderr
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
