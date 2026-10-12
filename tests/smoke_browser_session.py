"""Browser trust must survive service restart without weakening HTTP authorization."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from http.cookiejar import CookieJar


def main():
    binary = str(Path(os.environ.get("LAYA_TEST_BINARY", "target/debug/laya")).resolve())
    with tempfile.TemporaryDirectory(prefix="laya-browser-session-", dir="/private/tmp") as directory:
        root = Path(directory)
        environment = {**os.environ, "LAYA_WORKBENCH_DIR": directory, "LAYA_PORT": "0", "LAYA_NATIVE_USAGE_SYNC": "0"}
        process = None

        def start():
            nonlocal process
            process = subprocess.Popen([binary, "service"], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError(process.stderr.read().decode())
                try:
                    info = json.loads((root / "service.json").read_text())
                    if info["pid"] == process.pid:
                        environment["LAYA_PORT"] = str(info["port"])
                        return f"http://127.0.0.1:{info['port']}"
                except (FileNotFoundError, ValueError):
                    pass
                time.sleep(0.05)
            raise AssertionError("service did not start")

        def rpc(method):
            with socket.socket(socket.AF_UNIX) as channel:
                channel.connect(str(root / "service.sock"))
                channel.sendall(json.dumps({"protocol_version": 1, "request_id": "auth-test", "method": method, "params": {}}).encode() + b"\n")
                result = json.loads(channel.makefile("rb").readline())
            assert "error" not in result, result
            return result["result"]

        jar = CookieJar()
        browser = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))

        def request(path, body=None, origin_override=None, opener=browser):
            request = urllib.request.Request(origin + "/api/v1/" + path,
                data=json.dumps(body).encode() if body is not None else None,
                headers={"Origin": origin_override or origin, "Content-Type": "application/json"})
            with opener.open(request, timeout=5) as response:
                return json.loads(response.read())

        def denied(action, status):
            try:
                action()
                raise AssertionError("unauthorized request accepted")
            except urllib.error.HTTPError as error:
                assert error.code == status, error.code

        try:
            origin = start()
            denied(lambda: request("settings"), 401)
            code = urllib.parse.parse_qs(urllib.parse.urlparse(rpc("pair")["url"]).fragment)["pair"][0]
            assert request("pair", {"code": code})["paired"]
            denied(lambda: request("pair", {"code": code}), 401)
            token = next(iter(jar)).value
            session_file = root / "browser-sessions.json"
            assert token not in session_file.read_text()
            assert session_file.stat().st_mode & 0o777 == 0o600
            request("settings")
            request("settings")  # Ordinary refresh already works before restart.
            denied(lambda: request("settings", {}, "https://attacker.invalid"), 403)
            rpc("stop")
            process.wait(timeout=10)
            origin = start()
            request("settings")  # Regression: previously HTTP 401 after every upgrade/restart.
            assert request("session", {})["paired"]
            denied(lambda: request("session", {}, "https://attacker.invalid"), 403)
            denied(lambda: request("settings", opener=urllib.request.build_opener()), 401)
            forged = urllib.request.Request(origin + "/api/v1/settings", headers={"Cookie": "laya_session=forged"})
            denied(lambda: urllib.request.urlopen(forged), 401)
            revoke = urllib.request.Request(origin + "/api/v1/session", headers={"Origin": origin}, method="DELETE")
            browser.open(revoke).close()
            replay = urllib.request.Request(origin + "/api/v1/settings", headers={"Cookie": "laya_session=" + token})
            denied(lambda: urllib.request.urlopen(replay), 401)
            rpc("stop")
            process.wait(timeout=10)
            origin = start()
            denied(lambda: urllib.request.urlopen(replay), 401)
            print("PASS: paired browser survives service restart; anonymous and foreign-origin requests rejected")
        finally:
            if process and process.poll() is None:
                process.terminate()
                process.wait(timeout=10)


if __name__ == "__main__":
    main()
