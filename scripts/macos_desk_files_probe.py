"""Short macOS direct HTTPS Desk file check; use the browser driver for sustained load."""
import argparse
import base64
import concurrent.futures
import hashlib
import http.cookiejar
import json
from pathlib import Path
import ssl
import time
import urllib.error
import urllib.request
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--large-mib", type=int, default=1)
    parser.add_argument("--small-count", type=int, default=8)
    args = parser.parse_args()
    root = Path("/Library/Application Support/yoodesk-host")
    config = json.loads((root / "config.json").read_text())
    origin = config["public_origin"]
    prefix = "/" + config["product_id"] + "/webclient"
    tls = ssl.create_default_context(cafile=str(root / config["certificate"]))
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=tls),
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()),
    )

    def call(route, method="GET", body=None, csrf=None):
        headers = {"Origin": origin, "X-Request-ID": str(uuid.uuid4())}
        if body is not None:
            headers["Content-Type"] = "application/json"
        if csrf:
            headers["X-CSRF-Token"] = csrf
        request = urllib.request.Request(
            origin + prefix + route, headers=headers, method=method,
            data=json.dumps(body).encode() if body is not None else None,
        )
        try:
            with opener.open(request, timeout=20) as reply:
                return json.load(reply)["data"] if reply.status != 204 else None
        except urllib.error.HTTPError as e:
            reason = json.load(e).get("error", {}).get("message", "")
            raise RuntimeError(f"{(body or {}).get('action', method)}: HTTP {e.code}: {reason}") from None

    access = "/access/v2/"
    bootstrap = call(access + "bootstrap/")
    login = call(access + "login/", "POST", {
        "credential": (root / config["access_key_file"]).read_text().strip(),
        "access_mode": "cookie",
    }, bootstrap["preauth_csrf"])
    csrf = login["csrf_token"]
    sessions = "/desk/api/v2/sessions/"
    # 独立会话保持活跃，建立自己的受权文件操作上下文。
    session = call(sessions, "POST", {
        "protocol_version": 2, "target_id": "local", "options": {}, "client_features": ["h264s"],
    }, csrf)
    route = sessions + session["business_session_id"] + "/"

    def endpoint(action, **fields):
        return call(route + "endpoint/", "POST", {"action": action, **fields}, csrf)

    dirname = "Users/coder/YooDesk-macos-lab/artifacts/yoodesk-file-probe-" + uuid.uuid4().hex
    location = {"root_id": "system", "path": dirname}

    def transfer(name, size):
        fields = {"root_id": "system", "path": dirname + "/" + name}
        upload = endpoint("files.upload", **fields, size=size)["transfer_id"]
        expected = hashlib.sha256()
        try:
            for offset in range(0, size, 65536):
                block = hashlib.shake_256(f"{name}:{offset}".encode()).digest(min(65536, size - offset))
                expected.update(block)
                receipt = endpoint("files.write", transfer_id=upload, offset=offset,
                                   data=base64.b64encode(block).decode())
                assert receipt["offset"] == offset + len(block)
            assert endpoint("files.commit", transfer_id=upload)["name"] == name
        except BaseException:
            try:
                endpoint("files.cancel", transfer_id=upload)
            except RuntimeError:
                pass
            raise
        download = None
        try:
            download = endpoint("files.download", **fields)
            assert download["size"] == size
            received = hashlib.sha256()
            offset = 0
            while True:
                block = endpoint("files.read", transfer_id=download["transfer_id"], offset=offset)
                data = base64.b64decode(block["data"], validate=True)
                offset += len(data)
                assert offset == block["offset"]
                received.update(data)
                if block["done"]:
                    break
            assert offset == size and received.digest() == expected.digest()
            return size
        finally:
            if download is not None:
                try:
                    endpoint("files.cancel", transfer_id=download["transfer_id"])
                except RuntimeError:
                    pass
            try:
                endpoint("files.delete", **fields, kind="file")
            except RuntimeError as error:
                print(f"File cleanup: {error}", flush=True)

    try:
        for _ in range(20):
            if call(route).get("stream_available"):
                break
            time.sleep(.25)
        else:
            raise RuntimeError("Desk never produced a first frame")
        endpoint("files.create", **location, kind="directory")
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                small = list(pool.map(lambda i: transfer(f"small-{i}.bin", 4096), range(args.small_count)))
            print(json.dumps({"path": "lan_direct", "small_files": len(small), "parallel": 4,
                              "small_sha256": True}), flush=True)
            if args.large_mib:
                size = transfer("large.bin", args.large_mib * 1024**2)
                print(json.dumps({"path": "lan_direct", "large_bytes_each_direction": size,
                                  "large_sha256": True}), flush=True)
        finally:
            try:
                endpoint("files.delete", **location, kind="directory")
            except RuntimeError as error:
                print(f"Directory cleanup: {error}", flush=True)
    finally:
        call(route, "DELETE", csrf=csrf)


if __name__ == "__main__":
    main()
