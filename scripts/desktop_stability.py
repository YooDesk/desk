"""Prepare a native desktop fixture and run the browser pressure test."""

import argparse
import base64
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import time
import uuid


def powershell(script):
    subprocess.run(
        [
            "powershell.exe",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            base64.b64encode(
                ("$ProgressPreference='SilentlyContinue'\n" + script).encode("utf-16le")
            ).decode("ascii"),
        ],
        check=True,
        timeout=30,
    )


def ps_literal(value):
    return "'" + str(value).replace("'", "''") + "'"


def samples(file):
    if not file.exists():
        return []
    # The interactive task may still be writing the final line.
    with file.open(encoding="ascii") as stream:
        return [json.loads(line) for line in stream if line.endswith("\n")]


@contextmanager
def windows_stage():
    # Inherit the desktop user's LocalAppData ACL. An elevated Python-created
    # 0700 directory can be owned by Administrators and deny the limited task.
    stage = Path(os.environ["LOCALAPPDATA"]) / f"YooDesk-Stability-{uuid.uuid4().hex}"
    stage.mkdir()
    try:
        yield stage
    finally:
        for name in (
            "windows-motion.ps1",
            "motion-heartbeat.jsonl",
            "motion.log",
            "motion.json",
            "motion.stop",
        ):
            (stage / name).unlink(missing_ok=True)
        stage.rmdir()


def run_browser(args, node, runner, config, report, label, env=None):
    subprocess.run(
        [
            node,
            str(runner),
            str(config),
            str(report),
            label,
            str(args.cycles),
            str(args.full_cycles),
        ],
        check=True,
        timeout=900,
        env=env,
    )


def linux_worker(service):
    main = int(
        subprocess.check_output(
            ["systemctl", "show", service, "-p", "MainPID", "--value"],
            text=True,
            timeout=10,
        )
    )
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            if b"--rustdesk-worker" not in (path / "cmdline").read_bytes().split(b"\0"):
                continue
            status = dict(
                line.split(":", 1)
                for line in (path / "status").read_text().splitlines()
                if ":" in line
            )
            if int(status["PPid"]) != main:
                continue
            uid = int(status["Uid"].split()[0])
            env = dict(
                value.split("=", 1)
                for value in (path / "environ").read_text().split("\0")
                if "=" in value
            )
            if uid:
                return uid, env
        except (FileNotFoundError, ProcessLookupError):
            continue
    raise RuntimeError("No active graphical worker for this service")


def replace_config(path, content, metadata):
    fd, name = tempfile.mkstemp(prefix=".stability-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(content)
            os.fchown(stream.fileno(), metadata.st_uid, metadata.st_gid)
            os.fchmod(stream.fileno(), stat.S_IMODE(metadata.st_mode))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


def gtk_heartbeat(path):
    # The fixture runs as the desktop user; never follow its paths as root.
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > 8192:
            raise RuntimeError("Invalid GTK heartbeat")
        value = json.loads(stream.read(8193))
    return {key: int(value.get(key, 0)) for key in ("animationTicks", "monotonicMs")}


def run_linux(args, node, runner, config, report, motion_report):
    import pwd

    if os.geteuid() != 0:
        raise RuntimeError("Run with sudo on the Linux target VM")
    fixture = Path(__file__).with_name("desktop_stability_gtk.py").resolve()
    if not fixture.is_file():
        raise RuntimeError("Missing desktop_stability_gtk.py")
    original = config.read_bytes()
    metadata = config.stat()
    candidate = None
    service = args.service or "yoodesk-webservice.service"

    def restart():
        subprocess.run(["systemctl", "restart", service], check=True, timeout=60)

    try:
        if args.backend in ("drm", "portal"):
            value = json.loads(original)
            requested = args.backend == "drm"
            if value.get("enable_drm", True) != requested:
                if config.is_symlink() or not stat.S_ISREG(metadata.st_mode):
                    raise RuntimeError(
                        "Backend switching requires a regular service config"
                    )
                value["enable_drm"] = requested
                new = json.dumps(value, ensure_ascii=False, indent=2).encode("utf-8")
                replace_config(config, new, metadata)
                candidate = new
                restart()
        deadline = time.monotonic() + 20
        while True:
            try:
                uid, desktop = linux_worker(service)
                break
            except RuntimeError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.5)
        selected = desktop.get("RUSTDESK_FORCED_DISPLAY_SERVER")
        backend = (
            "x11"
            if selected == "x11"
            else (
                "drm"
                if selected == "wayland"
                and desktop.get("YOODESK_ENGINE_DRM_ENABLED") == "1"
                else "portal" if selected == "wayland" else None
            )
        )
        if backend is None or args.backend not in ("auto", backend):
            raise RuntimeError(f"Requested {args.backend}; worker selected {backend}")
        user = pwd.getpwuid(uid)
        runtime = Path(f"/run/user/{uid}")
        if runtime.stat().st_uid != uid:
            raise RuntimeError("Graphical runtime directory owner mismatch")
        with tempfile.TemporaryDirectory(
            prefix="yoodesk-stability-", dir=runtime
        ) as directory:
            stage = Path(directory)
            os.chown(stage, uid, user.pw_gid)
            graphical = {
                key: desktop[key]
                for key in (
                    "DISPLAY",
                    "XAUTHORITY",
                    "WAYLAND_DISPLAY",
                    "DBUS_SESSION_BUS_ADDRESS",
                )
                if key in desktop
            }
            graphical.update(
                XDG_RUNTIME_DIR=str(runtime),
                GDK_BACKEND="x11" if backend == "x11" else "wayland",
                YOODESK_STABILITY_FIXTURE_DIR=directory,
            )
            child = subprocess.Popen(
                [
                    "runuser",
                    "-u",
                    user.pw_name,
                    "--",
                    "env",
                    *(f"{key}={value}" for key, value in graphical.items()),
                    "/usr/bin/python3",
                    str(fixture),
                ],
                stdin=subprocess.DEVNULL,
            )
            heartbeat = stage / "report.json"
            try:
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline and child.poll() is None:
                    if heartbeat.exists():
                        before = gtk_heartbeat(heartbeat)
                        if before.get("animationTicks", 0) >= 5:
                            break
                    time.sleep(0.25)
                else:
                    raise RuntimeError("GTK fixture did not animate")
                print(f"GTK motion verified; testing {backend}.", flush=True)
                env = dict(os.environ, YOODESK_STABILITY_FIXTURE_DIR=directory)
                if args.browsers_path:
                    env["PLAYWRIGHT_BROWSERS_PATH"] = args.browsers_path
                run_browser(args, node, runner, config, report, backend, env)
                after = gtk_heartbeat(heartbeat)
                if after.get("animationTicks", 0) <= before["animationTicks"]:
                    raise RuntimeError("GTK animation stalled")
                motion_report.write_text(
                    json.dumps({"before": before, "after": after}, indent=2),
                    encoding="utf-8",
                )
            finally:
                fd, command = tempfile.mkstemp(prefix="quit-", dir=stage)
                with os.fdopen(fd, "w", encoding="ascii") as stream:
                    json.dump({"seq": time.time_ns(), "op": "quit"}, stream)
                    os.fchmod(stream.fileno(), 0o644)
                os.replace(command, stage / "command.json")
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.terminate()
                    child.wait(timeout=10)
    finally:
        if candidate is not None:
            if config.read_bytes() != candidate:
                raise RuntimeError(
                    "Service config changed during test; preserving current edits"
                )
            replace_config(config, original, metadata)
            restart()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", default="node")
    parser.add_argument("--config", type=Path)
    parser.add_argument(
        "--report", type=Path, help="Defaults to a unique timestamped file"
    )
    parser.add_argument("--cycles", type=int, default=12)
    parser.add_argument("--full-cycles", type=int, default=12)
    parser.add_argument(
        "--backend", choices=("auto", "x11", "drm", "portal"), default="auto"
    )
    parser.add_argument("--service", help="Linux systemd unit")
    parser.add_argument("--browsers-path", help="Linux Playwright browser cache")
    args = parser.parse_args()
    if not all(1 <= value <= 100 for value in (args.cycles, args.full_cycles)):
        parser.error("Cycle counts must be 1..100")
    windows = os.name == "nt"
    config = args.config or (
        (
            Path(os.environ["ProgramData"])
            / "yoodesk-WebService/yoodesk-web-service/config.json"
        )
        if windows
        else Path("/etc/yoodesk/yoodesk-webservice/config.json")
    )
    config = config.absolute()
    runner = Path(__file__).with_name("desktop_stability.mjs").resolve()
    fixture = Path(__file__).with_name("desktop_stability_windows.ps1")
    node = shutil.which(args.node)
    if not node or not all(p.is_file() for p in (config, runner)):
        parser.error("Node, service config, and desktop_stability.mjs are required")
    report = (
        args.report
        or Path(
            f"desktop-stability-{time.strftime('%Y%m%d-%H%M%S')}-{uuid.uuid4().hex[:8]}.json"
        )
    ).resolve()
    motion_report = report.with_suffix(".motion.json")
    if report.exists() or motion_report.exists():
        parser.error("Choose a new report path to preserve previous results")
    report.parent.mkdir(parents=True, exist_ok=True)
    if not windows:
        run_linux(args, node, runner, config, report, motion_report)
        return
    if args.backend != "auto" or not fixture.is_file():
        parser.error(
            "Windows requires --backend auto and desktop_stability_windows.ps1"
        )
    task = ps_literal("YooDesk-Stability-" + uuid.uuid4().hex)
    with windows_stage() as stage:
        animation = stage / "windows-motion.ps1"
        shutil.copyfile(fixture, animation)
        heartbeat = stage / "motion-heartbeat.jsonl"
        log = stage / "motion.log"
        system = Path(os.environ["SystemRoot"]) / "System32"
        executable = system / "WindowsPowerShell/v1.0/powershell.exe"
        argument = ps_literal(
            f"/d /c {executable} -NoProfile -STA -WindowStyle Hidden "
            f'-ExecutionPolicy Bypass -File "{animation}" -FixtureDirectory "{stage}" '
            f'> "{log}" 2>&1'
        )
        try:
            powershell(f"""
$ErrorActionPreference='Stop'
$action=New-ScheduledTaskAction -Execute {ps_literal(system / "cmd.exe")} -Argument {argument} -WorkingDirectory {ps_literal(stage)}
$user=[Security.Principal.WindowsIdentity]::GetCurrent().Name
$principal=New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 16)
Register-ScheduledTask -TaskName {task} -Action $action -Principal $principal -Settings $settings | Out-Null
Start-ScheduledTask -TaskName {task}
""")
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                readings = samples(heartbeat)
                if readings and readings[-1]["pixelChanges"] >= 5:
                    before = readings[-1]["pixelChanges"]
                    break
                time.sleep(0.25)
            else:
                powershell(
                    f"Get-ScheduledTaskInfo -TaskName {task} | Select-Object LastTaskResult,LastRunTime"
                )
                detail = log.read_text(errors="replace")[-1500:] if log.exists() else ""
                raise RuntimeError(
                    f"No visible motion; check the interactive Windows desktop. {detail}"
                )
            print("Visible desktop motion verified; running pressure test.", flush=True)
            run_browser(args, node, runner, config, report, "windows-native")
            readings = samples(heartbeat)
            if not readings or readings[-1]["pixelChanges"] - before < 5:
                raise RuntimeError("Visible motion did not continue during the test")
        finally:
            (stage / "motion.stop").touch()
            try:
                powershell(f"""
if(Get-ScheduledTask -TaskName {task} -ErrorAction SilentlyContinue){{
 Stop-ScheduledTask -TaskName {task} -ErrorAction SilentlyContinue
 Unregister-ScheduledTask -TaskName {task} -Confirm:$false -ErrorAction Stop
}}
""")
            finally:
                motion_report.write_text(
                    json.dumps(samples(heartbeat), indent=2), encoding="utf-8"
                )


if __name__ == "__main__":
    main()
