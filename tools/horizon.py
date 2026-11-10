#!/usr/bin/env python3
"""Export pinned sources, apply the template bundle next to Baritone's own and build a native Horizon.

The helper's glue calls `crate::game::baritone::*` and `crate::game::baritone_command::*`, which exist
only once Baritone's bundle is applied to the same sources — so staging always co-applies it. Baritone's
bundle pins an older Horizon revision; the launcher rewrites bundle pins to its workspace's before
applying, and so does this script (Baritone's archive is verified against its release SHA-256 first).
"""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
COMPAT = json.loads((ROOT / "compatibility.json").read_text())
PIN = COMPAT["horizon_revision"]
MIXINS = Path(os.environ.get("HORIZON_MIXINS", ROOT.parent / "horizon-mixins")).resolve()

# Baritone, the mod this one drives: one release, one exact archive. The helper is compiled against
# its modules, so a different Baritone needs a re-check of every call below.
BARITONE_ID = "baritone"
HELPER_ID = "baritone_helper"
BARITONE_VERSION = "1.1.52"
BARITONE_URL = (f"https://github.com/horizon-rust/horizon-mods/releases/download/"
                f"baritone-{BARITONE_VERSION}/baritone-{BARITONE_VERSION}.hnmod")
BARITONE_SHA256 = "d46c541d1e673a95f343960b6fb1646a0669ec07df11958af97f1a76b3b14ffa"

# The mod's staged suite: the runtime's tests live in the client lib's game
# module (game.rs compiles into the lib target; the bin's own unittests are
# none). The glue has no tests of its own — it is compiled, then exercised
# in a game.
SUITES = [
    (["-p", "horizon-client", "--lib", "--no-default-features"],
     "game::baritone_helper::tests", []),
]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def stage(horizon, stages):
    if not (horizon / ".git").exists():
        raise ValueError("--horizon must be a Git checkout (including a worktree)")
    subprocess.run(["git", "-C", str(horizon), "cat-file", "-e", PIN + "^{commit}"], check=True, capture_output=True)
    inputs = {"revision": PIN.encode(), "export-schema": b"2"}
    signature = digest(b"".join(k.encode() + b"\0" + value for k, value in sorted(inputs.items())))
    stages.mkdir(parents=True, exist_ok=True)
    out = stages / signature[:20]
    if out.exists():
        marker = json.loads((out / ".horizon-baritone_helper.json").read_text())
        if marker["signature"] != signature:
            raise ValueError(f"stage marker mismatch: {out}")
        for path, expected in marker["files"].items():
            if digest((out / path).read_bytes()) != expected:
                raise ValueError(f"staged source was edited: {path}; remove this generated stage to rebuild it")
        return out, False
    temporary = Path(tempfile.mkdtemp(prefix=".mod-template-stage-", dir=stages))
    try:
        process = subprocess.Popen(["git", "-C", str(horizon), "archive", "--format=tar", PIN], stdout=subprocess.PIPE)
        try:
            with tarfile.open(fileobj=process.stdout, mode="r|") as archive:
                for member in archive:
                    archive.extract(member, temporary, filter="data")
        finally:
            process.stdout.close()
            code = process.wait()
        if code:
            raise ValueError(f"git archive exited {code}")
        for path, expected in COMPAT["source_sha256"].items():
            if digest((temporary / path).read_bytes()) != expected:
                raise ValueError(f"pinned source hash mismatch: {path}")
        # Materialize the committed documentation symlink for the mixin CLI's
        # symlink-free source snapshot. No Rust source is edited here.
        for file in temporary.rglob("*"):
            if file.is_symlink():
                content = file.read_bytes()
                file.unlink()
                file.write_bytes(content)
        (temporary / ".horizon-revision").write_text(PIN + "\n")
        # The source tree itself is deterministic. Cargo outputs live elsewhere.
        files = {str(p.relative_to(temporary)): digest(p.read_bytes())
                 for p in sorted(temporary.rglob("*")) if p.is_file()}
        (temporary / ".horizon-baritone_helper.json").write_text(json.dumps(
            {"signature": signature, "horizon_revision": PIN, "files": files}, sort_keys=True, indent=2) + "\n")
        os.rename(temporary, out)
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise
    return out, True


def baritone_bundle(stages):
    """Baritone's .hnmod, verified byte for byte; env override, cache, then the release."""
    vendor = ROOT / "target/vendor"
    vendor.mkdir(parents=True, exist_ok=True)
    cache = vendor / f"{BARITONE_ID}-{BARITONE_VERSION}.hnmod"
    if env := os.environ.get("BARITONE_HNMOD"):
        source = Path(env)
        if digest(source.read_bytes()) != BARITONE_SHA256:
            raise ValueError(f"BARITONE_HNMOD is not Baritone {BARITONE_VERSION}: {source}")
        return source
    if cache.exists():
        if digest(cache.read_bytes()) != BARITONE_SHA256:
            cache.unlink()
        else:
            return cache
    if not cache.exists():
        print(f"fetching {BARITONE_URL}", file=sys.stderr)
        with urllib.request.urlopen(BARITONE_URL, timeout=120) as reply:
            data = reply.read()
        if digest(data) != BARITONE_SHA256:
            raise ValueError(f"downloaded Baritone does not match the pinned SHA-256: {BARITONE_SHA256}")
        cache.write_bytes(data)
    return cache


def baritone_stage(stages):
    """Baritone's bundle unpacked into `stages/baritone`, its pins rewritten to this pin.

    The archive packs the bundle as `mixins/<id>/…` (plus the package manifest); the mixin CLI's
    `--mods` dir takes one bundle per subdir, so the members are re-rooted. The archive is untrusted
    data: each member's name is checked to stay inside the destination before anything is written.
    """
    prefix = f"mixins/{BARITONE_ID}/"
    out = stages / "baritone"
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    archive_path = baritone_bundle(stages)
    with zipfile.ZipFile(archive_path) as archive:
        for info in archive.infolist():
            if info.is_dir():
                continue
            name = info.filename
            if not name.startswith(prefix):
                if name == "horizon-native.json":
                    continue
                raise ValueError(f"unexpected member in {archive_path.name}: {name}")
            relative = Path(name)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError(f"unsafe member name: {name}")
            if info.file_size > 32 << 20:
                raise ValueError(f"member too large: {name}")
            target = out / relative.relative_to(prefix)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.read(info))
    if not (out / "horizon-mixin.toml").exists():
        raise ValueError(f"{archive_path.name} holds no {prefix}horizon-mixin.toml")
    # The launcher's rewrite, done by hand: this stage is built on this pin,
    # not on the revision Baritone's bundle was written against.
    manifest = out / "horizon-mixin.toml"
    text = manifest.read_text()
    needle = "horizon_revision"
    lines = [line for line in text.splitlines() if needle in line]
    if len(lines) != 1:
        raise ValueError(f"{manifest}: expected exactly one {needle}")
    manifest.write_text(text.replace(lines[0], f'{needle} = "{PIN}"'))
    return out, digest(archive_path.read_bytes())


def mixin_stage(horizon, stages):
    revision = subprocess.check_output(["git", "-C", str(MIXINS), "rev-parse", "HEAD"], text=True).strip()
    if revision != COMPAT["horizon_mixins_revision"]:
        raise ValueError(f"horizon-mixins must be pinned at {COMPAT['horizon_mixins_revision']}, got {revision}")
    subprocess.run(["git", "-C", str(MIXINS), "diff", "--quiet", "HEAD", "--", "Cargo.toml", "Cargo.lock", "crates"], check=True)
    original, _ = stage(horizon, stages / "original")
    baritone, baritone_sha = baritone_stage(stages)
    # One mods dir with both bundles: the helper's glue calls Baritone's
    # modules, so the two are applied together or not at all.
    mods = stages / "all-mods"
    shutil.rmtree(mods, ignore_errors=True)
    mods.mkdir(parents=True)
    shutil.copytree(ROOT / "mixins" / HELPER_ID, mods / HELPER_ID)
    shutil.move(str(baritone), str(mods / BARITONE_ID))
    subprocess.run(["cargo", "build", "--locked", "--offline", "--target-dir", str(MIXINS / "target"), "-p", "horizon-mixins-cli"], cwd=MIXINS, check=True, stdout=sys.stderr)
    cli = MIXINS / "target/debug/horizon-mixins"
    output = stages / "modded"
    plan = stages / "plan.json"
    base = ["--workspace", str(original), "--mods", str(mods)]
    subprocess.run([str(cli), "plan", *base, "--out", str(plan)], check=True, capture_output=True, text=True)
    applied = subprocess.run([str(cli), "apply", *base, "--out", str(output)], check=True, capture_output=True, text=True)
    report = json.loads(applied.stdout)
    # The CLI drops a bundle whose targets no longer resolve and still says ok — the helper's glue
    # would then compile against nothing. Both modules must be in the staged sources, or stop here.
    staged_game_path = output / "crates/horizon-client/src/game.rs"
    staged_game = staged_game_path.read_text()
    for module in ("baritone", HELPER_ID):
        if f"pub(crate) mod {module} {{" not in staged_game:
            raise ValueError(f"the staged sources hold no `{module}` module: Baritone's bundle did not apply")
    # Baritone's bundle's own test code imports its modules the old crate-root way
    # (`crate::baritone_…`); at this pin game.rs compiles inside the library crate and they are
    # `crate::game::baritone_…`. The regular build never sees those lines (`cfg(test)`); the staged
    # test build does, so the imports are fixed here, in the stage only.
    fixed = staged_game.replace("use crate::baritone_", "use crate::game::baritone_")
    if fixed != staged_game:
        staged_game_path.write_text(fixed)
    report["baritone_sha256"] = baritone_sha
    return output, report["stage_changed"]


@contextlib.contextmanager
def lock(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        yield


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["stage", "check", "build", "test"])
    parser.add_argument("--horizon", type=Path, default=ROOT.parent / "horizon")
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target/horizon-cargo")
    args = parser.parse_args()
    horizon = args.horizon.resolve()
    target = args.target_dir.resolve()
    if target.is_relative_to(horizon):
        parser.error("Cargo target must be outside the original Horizon checkout")
    started = time.monotonic()
    with lock(ROOT / "target/stage.lock"):
        output, changed = mixin_stage(horizon, ROOT / "target/mixins")
        if target.is_relative_to(output) or output.is_relative_to(target):
            parser.error("Cargo target and generated source stage must be separate")
        report = {"stage": str(output), "stage_changed": changed,
                  "horizon_revision": PIN, "staging_seconds": time.monotonic()-started}
        if args.action != "stage":
            if args.action == "test":
                report["suites"] = []
                for package, suite_filter, tail in SUITES:
                    command = ["cargo", "test", "--locked", "--offline", *package,
                               suite_filter, "--target-dir", str(target), *tail]
                    run_start = time.monotonic()
                    result = subprocess.run(command, cwd=output, capture_output=True, text=True)
                    suite_report = {"filter": suite_filter, "package": package[1],
                                    "cargo_seconds": time.monotonic()-run_start,
                                    "cargo_command": command, "cargo_exit": result.returncode}
                    log = ROOT / "target" / f"horizon-test-{package[1]}.log"
                    log.write_text(result.stdout + result.stderr)
                    suite_report["cargo_log"] = str(log)
                    report["suites"].append(suite_report)
                    if result.returncode:
                        print(result.stderr[-12000:], file=sys.stderr)
                        print(json.dumps(report), file=sys.stderr)
                        return result.returncode
            else:
                command = ["cargo", args.action, "--locked", "--offline", "-p", "horizon-client",
                           "--bin", "horizon-client", "--no-default-features", "--target-dir", str(target)]
                if args.action == "build":
                    command.extend(["--profile", "fast"])
                with lock(target.parent / (target.name + ".mod-template.lock")):
                    run_start = time.monotonic()
                    result = subprocess.run(command, cwd=output, capture_output=True, text=True)
                report.update(cargo_seconds=time.monotonic()-run_start, cargo_command=command,
                              cargo_exit=result.returncode)
                log = ROOT / "target" / f"horizon-{args.action}.log"
                log.write_text(result.stdout + result.stderr)
                report["cargo_log"] = str(log)
                if result.returncode:
                    print(result.stderr[-12000:], file=sys.stderr)
                    print(json.dumps(report), file=sys.stderr)
                    return result.returncode
                if args.action == "build":
                    report["binary"] = str(target / "fast/horizon-client")
        print(json.dumps(report, indent=2))
        return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        print(f"Native mod: {error}", file=sys.stderr)
        raise SystemExit(1)
