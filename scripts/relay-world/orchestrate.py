#!/usr/bin/env python3
"""relay-world orchestrator: net-probe on Fly Machines around the world.

Run it through run.sh (token, hard cap, watchdog, cleanup trap). It:
  1. starts one shared-cpu-1x machine per region in the partii-relay-test app,
  2. runs the cases (each a series of sub-runs: one host probe + a member
     probe on every other machine, all in one STAGING star room),
  3. writes <out>/results.json, <out>/report.json, <out>/report.md,
  4. destroys every machine in the app (also on any failure).

Progress: <out>/progress.jsonl gets one line per finished case;
<out>/heartbeat gets a line per step (run.sh's watchdog watches it).
"""

import argparse
import concurrent.futures as cf
import json
import os
import shlex
import signal
import subprocess
import sys
import threading
import time

APP = "partii-relay-test"
STAGING = "https://partii-edge-staging.partii-games.workers.dev"
# Fly shared-cpu-1x 256 MB is ~$0.0027/h in most regions; gru/syd/jnb/sin/nrt
# cost a little more. Budget conservatively.
MACHINE_USD_PER_HOUR = 0.004
CONTINENT = {
    "iad": "NA", "sjc": "NA", "gru": "SA", "lhr": "EU", "fra": "EU",
    "sin": "AS", "nrt": "AS", "syd": "OC", "jnb": "AF",
}

OUT = "."
LOCK = threading.Lock()
MACHINES = {}  # region -> machine id
MACHINE_SECONDS = {"started": {}, "total": 0.0}


def log(msg):
    line = f"[{time.strftime('%H:%M:%S')}] {msg}"
    print(line, flush=True)
    with LOCK, open(os.path.join(OUT, "heartbeat"), "a") as f:
        f.write(line + "\n")


def progress(case, **kw):
    with LOCK, open(os.path.join(OUT, "progress.jsonl"), "a") as f:
        f.write(json.dumps({"t": time.strftime("%Y-%m-%dT%H:%M:%S"), "case": case, **kw}) + "\n")


def fly(args, timeout=60, check=True):
    """flyctl with a hard timeout. The token comes from FLY_API_TOKEN (never printed)."""
    try:
        p = subprocess.run(["flyctl", *args], capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        if check:
            raise RuntimeError(f"flyctl {args[0]} {args[1] if len(args) > 1 else ''}: timed out after {timeout}s")
        return None
    if check and p.returncode != 0:
        raise RuntimeError(f"flyctl {' '.join(args[:2])}: rc={p.returncode}: {(p.stderr or p.stdout)[-400:]}")
    return p


def exec_on(region, cmd, timeout=40, tries=3):
    """Run a command on a machine. Execs flake now and then (ECHILD from the
    machine's exec agent); every command sent here is safe to repeat."""
    mid = MACHINES[region]
    err = None
    for i in range(tries):
        p = fly(["machine", "exec", mid, cmd, "-a", APP, "--timeout", str(timeout)], timeout=timeout + 20, check=False)
        if p is None:
            err = "timeout"
        elif p.returncode == 0:
            return p.stdout, None
        else:
            err = (p.stderr or p.stdout or "")[-300:]
            # A command that ran and failed (test -f before .done): no retry.
            if "could not exec" not in err and "failed_precondition" not in err:
                return p.stdout, err
        time.sleep(1 + i)
    return None, err


def list_machines():
    p = fly(["machine", "list", "-a", APP, "--json"], timeout=60, check=False)
    if p is None or p.returncode != 0:
        return []
    try:
        return json.loads(p.stdout or "[]")
    except json.JSONDecodeError:
        return []


def start_machines(image, regions, edge):
    def one(r):
        t = time.time()
        p = fly([
            "machine", "run", image, "-a", APP, "--region", r, "--name", f"probe-{r}",
            "--vm-size", "shared-cpu-1x", "--vm-memory", "256", "--restart", "no",
            "--env", f"PARTII_EDGE={edge}", "--detach",
        ], timeout=180, check=False)
        if p is None or p.returncode != 0:
            log(f"machine in {r} failed: {(p.stderr if p else 'timeout')[-300:]}")
            return r, False
        with LOCK:
            MACHINE_SECONDS["started"][r] = t
        return r, True

    with cf.ThreadPoolExecutor(len(regions)) as ex:
        ok = [r for r, good in ex.map(one, regions) if good]
    # Map regions to ids and wait for them to be started.
    deadline = time.time() + 120
    while time.time() < deadline:
        ms = list_machines()
        for m in ms:
            if m.get("state") == "started" and m.get("region") in ok:
                MACHINES[m["region"]] = m["id"]
        if all(r in MACHINES for r in ok):
            break
        time.sleep(3)
    log(f"machines up: {sorted(MACHINES)} (asked {regions})")
    # The probe's env file is written by the entrypoint; check the key arrived.
    for r in list(MACHINES):
        out, err = exec_on(r, "sh -c 'grep -c PARTII_KEY /run/probe.env'", timeout=20)
        if not out or out.strip() != "1":
            log(f"{r}: no PARTII_KEY in the machine env ({err}); dropping it")
            MACHINES.pop(r, None)


def destroy_all():
    ms = list_machines()
    for m in ms:
        fly(["machine", "destroy", m["id"], "-a", APP, "--force"], timeout=60, check=False)
    now = time.time()
    with LOCK:
        for r, t in MACHINE_SECONDS["started"].items():
            MACHINE_SECONDS["total"] += now - t
        MACHINE_SECONDS["started"] = {}
    left = [m["id"] for m in list_machines() if m.get("state") not in ("destroyed",)]
    log(f"destroyed {len(ms)} machines; left: {left}")
    return left


def parse_result(out):
    """A finished probe's JSON (dict), an error string, or None (not done yet)."""
    lines = [l for l in (out or "").splitlines() if l.strip()]
    if not lines or lines[-1].strip() == "WAIT":
        return None
    for l in reversed(lines):
        l = l.strip()
        if l.startswith("{"):
            try:
                return json.loads(l)
            except json.JSONDecodeError:
                break
    return f"no JSON from the probe: {(out or '')[-300:]}"


def run_probe(region, label, args, secs):
    """Start net-probe on a machine and wait for its JSON."""
    cmd = "probe.sh " + shlex.quote(label) + " " + " ".join(shlex.quote(a) for a in args)
    out, err = exec_on(region, cmd, timeout=30)
    if not out or "started" not in out:
        return {"region": region, "label": label, "errors": [f"start: {err}"]}
    deadline = time.time() + secs + 40
    while time.time() < deadline:
        time.sleep(4)
        out, err = exec_on(region, f"sh -c 'if test -f /tmp/{label}.done; then cat /tmp/{label}.json; else echo WAIT; fi'", timeout=20)
        got = parse_result(out)
        if got is not None:
            return got if isinstance(got, dict) else {"region": region, "label": label, "errors": [got]}
    tail, _ = exec_on(region, f"sh -c 'tail -c 600 /tmp/{label}.log'", timeout=20)
    return {"region": region, "label": label, "errors": [f"no result in {secs + 40}s", tail or ""]}


def subrun(case, host_region, members, mode, secs, extra=(), host_extra_secs=30):
    """One room: host on host_region, a member on each of `members`."""
    tag = f"{case}-{host_region}-{mode}".replace("_", "-")
    log(f"sub-run {tag}: host {host_region}, members {members}")
    hlabel = f"{tag}-host"
    hsecs = secs + host_extra_secs
    out, err = exec_on(host_region, "probe.sh " + hlabel + f" host --secs {hsecs} --code-file /tmp/{hlabel}.code", timeout=30)
    if not out or "started" not in out:
        return {"case": case, "host": host_region, "mode": mode, "error": f"host start: {err}", "members": []}
    code = None
    deadline = time.time() + 30
    while time.time() < deadline and not code:
        time.sleep(2)
        o, _ = exec_on(host_region, f"sh -c 'cat /tmp/{hlabel}.code 2>/dev/null'", timeout=15)
        code = (o or "").strip() or None
    if not code:
        tail, _ = exec_on(host_region, f"sh -c 'tail -c 600 /tmp/{hlabel}.log; cat /tmp/{hlabel}.json'", timeout=15)
        return {"case": case, "host": host_region, "mode": mode, "error": f"no room code: {tail}", "members": []}
    log(f"sub-run {tag}: room {code}")
    with cf.ThreadPoolExecutor(max(1, len(members))) as ex:
        futs = {
            ex.submit(run_probe, r, f"{tag}-{r}", ["member", "--code", code, "--path", mode, "--secs", str(secs), *extra], secs): r
            for r in members
        }
        results = [f.result() for f in futs]
    hres = run_probe_result(host_region, hlabel, hsecs)
    log(f"sub-run {tag}: done ({sum(1 for r in results if not r.get('errors'))}/{len(results)} clean)")
    return {"case": case, "host": host_region, "mode": mode, "code": code, "members": results, "host_result": hres}


def run_probe_result(region, label, secs):
    deadline = time.time() + secs + 20
    while time.time() < deadline:
        out, _ = exec_on(region, f"sh -c 'if test -f /tmp/{label}.done; then cat /tmp/{label}.json; else echo WAIT; fi'", timeout=20)
        got = parse_result(out)
        if got is not None:
            return got if isinstance(got, dict) else {"errors": [got]}
        time.sleep(4)
    return {"errors": ["host result timed out"]}


# ── Analysis ──

def p(m, path, key="p50"):
    return (((m or {}).get("paths") or {}).get(path) or {}).get(key)


def analyse(runs, regions, cap_s):
    cases = {}
    matrix = {}  # host -> member -> mode -> stats
    for run in runs:
        for m in run.get("members", []):
            mode = run["mode"]
            if run["case"] not in ("a", "d"):
                continue
            pathname = mode if mode != "auto" else (m.get("final_path") or "auto")
            cell = {
                "opened": bool(m.get("first_echo_ms")) and not m.get("errors") if mode == "auto" else (p(m, mode, "opened") and p(m, mode, "samples") or 0) > 0,
                "open_ms": (m.get("first_echo_ms") if mode == "auto" else p(m, mode, "open_ms")),
                "p50": p(m, pathname), "p95": p(m, pathname, "p95"), "p99": p(m, pathname, "p99"),
                "loss": p(m, pathname, "loss"),
                "final_path": m.get("final_path"),
                "ice": p(m, pathname, "ice"),
                "edge_colo": m.get("edge_colo"),
                "errors": m.get("errors"),
                "link_rtt": {k: (v.get("link_rtt") or 0) + (25 if k == "ws" else 0)
                             for k, v in (m.get("paths") or {}).items() if v.get("opened") and v.get("link_rtt")},
            }
            matrix.setdefault(run["host"], {}).setdefault(m.get("region") or "?", {})[mode] = cell

    # (a)+(d): every pair opens every forced path; auto picks the lowest p50;
    # turn p50 <= ws p50 cross-continent.
    def verdicts(hosts):
        pairs, fails = 0, []
        auto_ok = auto_n = 0
        turn_cmp = []
        for h in hosts:
            for r, modes in matrix.get(h, {}).items():
                pairs += 1
                for mode in ("ws", "turn", "direct", "auto"):
                    c = modes.get(mode)
                    if not c or not c["opened"]:
                        fails.append(f"{h}->{r} {mode} did not open" + (f" ({c['errors']})" if c and c.get("errors") else ""))
                forced = {k: modes[k]["p50"] for k in ("ws", "turn", "direct") if modes.get(k) and modes[k]["opened"] and modes[k]["p50"] is not None}
                a = modes.get("auto")
                # Judge auto against the paths it measured at the same time (its
                # link RTTs; ws with its 25 ms penalty): forced sub-runs are
                # minutes apart and Internet routes move (iad->sjc direct: 64 ms
                # in the auto run, 91 ms in the forced one).
                live = (a or {}).get("link_rtt") or {}
                if a and a.get("final_path") in live:
                    auto_n += 1
                    best = min(live.values())
                    got = live[a["final_path"]]
                    if got <= best + max(5.0, 0.1 * best):
                        auto_ok += 1
                    else:
                        fails.append(f"{h}->{r} auto settled on {a['final_path']} ({got} ms) but {min(live, key=live.get)} measured {best} ms")
                if CONTINENT.get(h) != CONTINENT.get(r) and "ws" in forced and "turn" in forced:
                    turn_cmp.append((h, r, forced["turn"], forced["ws"]))
        turn_worse = [f"{h}->{r} turn p50 {t} > ws p50 {w}" for h, r, t, w in turn_cmp if t > w]
        return {"pairs": pairs, "open_failures": [f for f in fails if "did not open" in f],
                "auto_picks_lowest": f"{auto_ok}/{auto_n}", "auto_failures": [f for f in fails if "auto settled" in f],
                "turn_vs_ws_cross_continent": f"{len(turn_cmp) - len(turn_worse)}/{len(turn_cmp)} turn <= ws",
                "turn_worse": turn_worse}

    hosts_a = [r["host"] for r in runs if r["case"] == "a"][:1]
    hosts_d = sorted({r["host"] for r in runs if r["case"] in ("a", "d")})
    for name, hosts in (("a", hosts_a), ("d", hosts_d)):
        v = verdicts(hosts)
        v["hosts"] = hosts
        v["pass"] = not v["open_failures"] and not v["auto_failures"] and not v["turn_worse"] and v["pairs"] > 0
        cases[name] = v

    # (b) UDP blocked: turn opens (TURN over TLS 443) on every member and
    # beats or matches ws.
    b_runs = [r for r in runs if r["case"] == "b"]
    if b_runs:
        rows, fails = [], []
        by = {}
        for run in b_runs:
            for m in run.get("members", []):
                by.setdefault(m.get("region"), {})[run["mode"]] = m
        ws_ref = {r: c.get("ws", {}).get("p50") for r, c in matrix.get(b_runs[0]["host"], {}).items()}
        for r, modes in sorted(by.items()):
            t = modes.get("turn")
            a = modes.get("auto")
            turn_open = bool(t and p(t, "turn", "samples"))
            row = {"region": r, "udp_blocked": bool(t and t.get("udp_blocked")),
                   "turn_opened": turn_open, "turn_p50": p(t, "turn") if t else None,
                   "turn_tls": bool(((p(t, "turn", "ice") or {}).get("local") or {}).get("tls_bridge")) if t else None,
                   "turn_open_ms": p(t, "turn", "open_ms") if t else None,
                   "auto_final": a.get("final_path") if a else None, "auto_p50": (a or {}).get("overall", {}).get("p50"),
                   "ws_p50_open_net": ws_ref.get(r)}
            rows.append(row)
            if not row["udp_blocked"]:
                fails.append(f"{r}: UDP was not blocked")
            if not turn_open:
                joined = bool(t and t.get("join_ms"))
                fails.append(f"{r}: turn did not open with UDP blocked" if joined
                             else f"{r}: never joined the room ({(t or {}).get('errors')}): an edge error, not a path")
            elif row["ws_p50_open_net"] is not None and row["turn_p50"] > row["ws_p50_open_net"] + 5:
                fails.append(f"{r}: turn p50 {row['turn_p50']} > ws p50 {row['ws_p50_open_net']}")
        cases["b"] = {"host": b_runs[0]["host"], "rows": rows, "failures": fails, "pass": not fails and bool(rows)}

    # (c) failover: play continues on ws within ~1 s, no ordered loss, then
    # promotes back.
    for case in ("c", "c_iptables"):
        c_runs = [r for r in runs if r["case"] == case]
        if not c_runs:
            continue
        rows, fails = [], []
        for run in c_runs:
            for m in run.get("members", []):
                f = m.get("failover") or {}
                rel = m.get("reliable") or {}
                row = {"region": m.get("region"), "before": f.get("path_before"), "switched_to": f.get("switched_to"),
                       "switch_ms": f.get("switch_ms"), "play_gap_ms": f.get("play_gap_ms"),
                       "promote_back_ms": f.get("promote_back_ms"), "promoted_to": f.get("promoted_to"),
                       "rel_missing": rel.get("missing"), "rel_out_of_order": rel.get("out_of_order"),
                       "rel_max_rtt_ms": rel.get("max_rtt_ms"), "errors": m.get("errors")}
                rows.append(row)
                r = row["region"]
                if row["before"] in (None, "ws"):
                    fails.append(f"{r}: was on {row['before']} before the blackhole (nothing to fail over)")
                    continue
                # App blackhole kills turn + direct: ws is the only way out. An
                # iptables UDP drop leaves TURN over TLS (TCP 443) alive too.
                ok_to = ("ws",) if case == "c" else ("ws", "turn")
                if row["switched_to"] not in ok_to:
                    fails.append(f"{r}: switched to {row['switched_to']}, not {' or '.join(ok_to)}")
                if row["play_gap_ms"] is None or row["play_gap_ms"] > 1000:
                    fails.append(f"{r}: play gap {row['play_gap_ms']} ms > 1000")
                if row["rel_missing"] or row["rel_out_of_order"]:
                    fails.append(f"{r}: reliable missing {row['rel_missing']} out of order {row['rel_out_of_order']}")
                if row["promote_back_ms"] is None:
                    fails.append(f"{r}: never promoted back off ws")
        cases[case] = {"host": c_runs[0]["host"], "rows": rows, "failures": fails, "pass": not fails and bool(rows)}
    return matrix, cases


def fmt(v, unit=""):
    if v is None:
        return "–"
    if isinstance(v, float):
        return f"{v:.0f}{unit}" if v >= 10 else f"{v:.1f}{unit}"
    return f"{v}{unit}"


def write_report(runs, regions, matrix, cases, cost, left, wall_s, meta):
    report = {"meta": meta, "cases": cases, "matrix": matrix, "cost": cost, "machines_left": left, "wall_s": round(wall_s)}
    report["cases"]["zero_machines_left"] = {"pass": not left, "left": left}
    with open(os.path.join(OUT, "report.json"), "w") as f:
        json.dump(report, f, indent=1)
    L = [f"# relay-world: {meta['when']}", "",
         f"Edge: {meta['edge']} · image {meta['image']} · regions {', '.join(meta['regions'])} · wall {round(wall_s / 60, 1)} min", ""]
    L += ["## Pass / fail", "", "| Case | Result | Notes |", "|---|---|---|"]
    names = {"a": "(a) host one region, members everywhere", "b": "(b) UDP blocked: turn over TLS 443",
             "c": "(c) failover (app blackhole of WebRTC)", "c_iptables": "(c') failover (iptables UDP drop)",
             "d": "(d) host rotated US/EU/Asia", "zero_machines_left": "zero machines left"}
    for k in ("a", "d", "b", "c", "c_iptables", "zero_machines_left"):
        c = cases.get(k)
        if not c:
            continue
        notes = []
        if k in ("a", "d"):
            notes.append(f"pairs {c['pairs']}, auto lowest {c['auto_picks_lowest']}, {c['turn_vs_ws_cross_continent']}")
            notes += c["open_failures"][:4] + c["auto_failures"][:3] + c["turn_worse"][:3]
        elif k == "zero_machines_left":
            notes.append(f"left: {c['left']}")
        else:
            notes += c["failures"][:6]
        L.append(f"| {names[k]} | {'PASS' if c['pass'] else 'FAIL'} | {'; '.join(notes)} |")
    L += [""]
    for h, rows in matrix.items():
        L += [f"## RTT matrix, host {h} (ms, p50 / p95 / p99; loss; open ms)", "",
              "| member | ws | turn | direct | auto (settled) | edge colo |", "|---|---|---|---|---|---|"]
        for r in sorted(rows):
            cells = []
            for mode in ("ws", "turn", "direct", "auto"):
                c = rows[r].get(mode)
                if not c:
                    cells.append("–")
                elif not c["opened"]:
                    cells.append("no open")
                else:
                    s = f"{fmt(c['p50'])} / {fmt(c['p95'])} / {fmt(c['p99'])}; {fmt((c['loss'] or 0) * 100, '%')}; {fmt(c['open_ms'])}"
                    if mode == "auto":
                        s += f" ({c['final_path']})"
                    cells.append(s)
            colo = (rows[r].get("ws") or rows[r].get("auto") or {}).get("edge_colo")
            L.append(f"| {r} | " + " | ".join(cells) + f" | {colo or '–'} |")
        L.append("")
    for k in ("b", "c", "c_iptables"):
        c = cases.get(k)
        if not c:
            continue
        L += [f"## {names[k]} (host {c['host']})", ""]
        if c["rows"]:
            cols = list(c["rows"][0].keys())
            L += ["| " + " | ".join(cols) + " |", "|" + "---|" * len(cols)]
            for row in c["rows"]:
                L.append("| " + " | ".join(fmt(row[k2]) if not isinstance(row[k2], (dict, list)) else json.dumps(row[k2])[:80] for k2 in cols) + " |")
        L.append("")
    L += ["## Cost", "", f"Machines: {cost['machine_hours']:.3f} machine-hours × ${MACHINE_USD_PER_HOUR}/h ≈ ${cost['machines_usd']:.3f}; "
          f"egress ≈ {cost['egress_gb']:.3f} GB ≈ ${cost['egress_usd']:.3f}; builder ≈ ${cost['builder_usd']:.2f}. "
          f"Total ≈ ${cost['total_usd']:.2f} (estimate).", ""]
    with open(os.path.join(OUT, "report.md"), "w") as f:
        f.write("\n".join(L))


def main():
    global OUT
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", required=True)
    ap.add_argument("--regions", default="iad,sjc,gru,lhr,fra,sin,syd,nrt,jnb")
    ap.add_argument("--hosts", default="iad,fra,sin", help="host regions for (a)/(d); the first is (a)'s")
    ap.add_argument("--cases", default="a,d,b,c,c_iptables")
    ap.add_argument("--secs", type=int, default=30, help="member seconds per matrix sub-run")
    ap.add_argument("--modes", default="auto,ws,turn,direct")
    ap.add_argument("--edge", default=os.environ.get("PARTII_EDGE", STAGING))
    ap.add_argument("--out", required=True)
    ap.add_argument("--builder-usd", type=float, default=0.05)
    ap.add_argument("--reanalyse", action="store_true", help="rebuild report.md/json from <out>/results.json (no machines)")
    a = ap.parse_args()
    if a.reanalyse:
        OUT = a.out
        runs = json.load(open(os.path.join(OUT, "results.json")))
        old = json.load(open(os.path.join(OUT, "report.json")))
        matrix, verdict = analyse(runs, [], 0)
        write_report(runs, [], matrix, verdict, old["cost"], old["machines_left"], old["wall_s"], old["meta"])
        return 0
    # run.sh's cap/watchdog sends SIGTERM: unwind through `finally` (destroy, report).
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    if "partii.games" in a.edge and "staging" not in a.edge:
        sys.exit("refusing a non-staging edge")
    OUT = a.out
    os.makedirs(OUT, exist_ok=True)
    t0 = time.time()
    regions = [r for r in a.regions.split(",") if r]
    hosts = [h for h in a.hosts.split(",") if h]
    cases = a.cases.split(",")
    modes = a.modes.split(",")
    runs = []
    left = ["unknown"]
    try:
        start_machines(a.image, regions, a.edge)
        live = sorted(MACHINES)
        if len(live) < 2:
            raise RuntimeError(f"not enough machines: {live}")
        hosts = [h for h in hosts if h in MACHINES] or live[:1]

        def matrix_case(case, host):
            members = [r for r in live if r != host]
            for mode in modes:
                runs.append(subrun(case, host, members, mode, a.secs))
                save(runs)

        if "a" in cases:
            matrix_case("a", hosts[0])
            progress("a", host=hosts[0], subruns=len(modes))
        if "d" in cases:
            for h in hosts[1:]:
                matrix_case("d", h)
            progress("d", hosts=hosts[1:], subruns=len(modes) * len(hosts[1:]))
        h0 = hosts[0]
        members0 = [r for r in live if r != h0]
        if "b" in cases:
            for mode in ("turn", "auto"):
                runs.append(subrun("b", h0, members0, mode, a.secs, extra=["--block-udp"]))
                save(runs)
            progress("b", host=h0)
        if "c" in cases:
            runs.append(subrun("c", h0, members0, "auto", 60,
                               extra=["--blackhole", "webrtc", "--blackhole-at", "20", "--restore-at", "35"]))
            save(runs)
            progress("c", host=h0)
        if "c_iptables" in cases:
            # 75 s: a real outage kills the pcs, so promoting back needs a rebuild.
            runs.append(subrun("c_iptables", h0, members0, "auto", 75,
                               extra=["--blackhole", "webrtc", "--blackhole-mode", "iptables", "--blackhole-at", "20", "--restore-at", "35"]))
            save(runs)
            progress("c_iptables", host=h0)
        for case in [c for c in cases if c.startswith("j")]:
            # tankii's flapping check: alive-but-jittery UDP (stalls of N ms
            # every 3-5 s, 2% loss) for ~2 min; "j450" stalls 450 ms.
            stall = case[1:] or "450"
            runs.append(subrun(case, h0, members0, "auto", 150, extra=["--jitter", stall, "--jitter-from", "10"]))
            save(runs)
            progress(case, host=h0)
    except (Exception, KeyboardInterrupt) as e:  # noqa: BLE001 - the run must still clean up and report
        log(f"run failed: {e}")
        progress("error", error=str(e)[:500])
    finally:
        left = destroy_all()
    wall = time.time() - t0
    mh = MACHINE_SECONDS["total"] / 3600
    # ~30 Hz × 2 directions × ~120 B on the wire per member, plus pings.
    member_secs = sum(len(r.get("members", [])) * (150 if r["case"].startswith("j") else 75 if r["case"].startswith("c") else a.secs) for r in runs)
    egress_gb = member_secs * 30 * 2 * 160 * 1.5 / 1e9
    cost = {"machine_hours": mh, "machines_usd": mh * MACHINE_USD_PER_HOUR, "egress_gb": egress_gb,
            "egress_usd": egress_gb * 0.04, "builder_usd": a.builder_usd}
    cost["total_usd"] = cost["machines_usd"] + cost["egress_usd"] + cost["builder_usd"]
    matrix, verdict = analyse(runs, regions, 1800)
    meta = {"when": time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime(t0)), "edge": a.edge, "image": a.image,
            "regions": sorted(MACHINES) or regions}
    write_report(runs, regions, matrix, verdict, cost, left, wall, meta)
    progress("done", wall_s=round(wall), machines_left=len(left), report=os.path.join(OUT, "report.md"))
    log(f"report: {os.path.join(OUT, 'report.md')}")
    return 0 if not left else 3


def save(runs):
    with open(os.path.join(OUT, "results.json"), "w") as f:
        json.dump(runs, f)


if __name__ == "__main__":
    sys.exit(main())
