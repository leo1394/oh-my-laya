"""Opt-in 60-second release idle measurement; no model is loaded."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]


def sample(pid):
    values = subprocess.check_output(
        ["ps", "-p", str(pid), "-o", "rss=", "-o", "time="], text=True
    ).split()
    parts = [float(part) for part in values[1].split(":")]
    seconds = 0
    for part in parts:
        seconds = seconds * 60 + part
    return {"rss_kib": int(values[0]), "cpu_seconds": seconds}


def main():
    binary = ROOT / "target/release/laya"
    with tempfile.TemporaryDirectory(prefix="laya-resource-", dir="/private/tmp") as directory:
        env = {**os.environ, "LAYA_WORKBENCH_DIR": directory}
        service = subprocess.Popen([str(binary), "service"], env=env)
        bridges = []
        try:
            for _ in range(100):
                if (Path(directory) / "service.json").exists():
                    break
                time.sleep(0.05)
            for index in range(3):
                bridge = subprocess.Popen([str(binary), "mcp"], env=env, stdin=subprocess.PIPE,
                                          stdout=subprocess.PIPE, text=True)
                bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "id": index,
                                               "method": "initialize", "params": {}}) + "\n")
                bridge.stdin.flush()
                assert json.loads(bridge.stdout.readline())["result"]["serverInfo"]["name"] == "oh-my-laya"
                bridges.append(bridge)
            start = sample(service.pid)
            started = time.monotonic()
            peak = start["rss_kib"]
            for _ in range(60):
                time.sleep(1)
                peak = max(peak, sample(service.pid)["rss_kib"])
            final = sample(service.pid)
            duration = time.monotonic() - started
            print(json.dumps({"duration_seconds": round(duration, 2), "service_peak_rss_kib": peak,
                              "service_single_core_cpu_percent": round(100 * (final["cpu_seconds"] - start["cpu_seconds"]) / duration, 4),
                              "mcp_bridges": [sample(bridge.pid) for bridge in bridges],
                              "scope": "macOS ps RSS, separate processes; no Python model or browser; not summed unified memory"}))
        finally:
            for bridge in bridges:
                bridge.stdin.close()
                bridge.wait(timeout=5)
            subprocess.run([str(binary), "stop"], env=env, capture_output=True, timeout=10)
            try:
                service.wait(timeout=10)
            except subprocess.TimeoutExpired:
                service.terminate()
                service.wait(timeout=5)


if __name__ == "__main__":
    main()
