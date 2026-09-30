#!/usr/bin/env python3
"""Load the example control systems into an Unlinked instance.

Creates one organization with a project per topic, uploads every example
(models and init scripts) and runs each model once with its documented
solver settings, so the demo opens with results already in the run history.

Uses only the public HTTP API. Sign-in is dev login, so the server must run
with --dev-mode; against a production server pass --session with the value
of a signed-in `session` cookie instead.

    scripts/seed-demo.py --url http://localhost:3000
"""

import argparse
import http.cookiejar
import json
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request

EXAMPLES = pathlib.Path(__file__).resolve().parent.parent / "examples"

# Project -> [(file, solver settings or None for scripts, init script, note)].
PROJECTS = {
    "Inverted pendulum": {
        "description": "Balancing an inverted pendulum: LQR state feedback on "
        "the linearized cart-pole, and a saturated PD controller on the "
        "nonlinear pendulum.",
        "files": [
            ("inverted_pendulum_lqr.mdl", dict(stop=5, step=0.01), None),
            ("inverted_pendulum_nonlinear_pd.mdl", dict(stop=4, step=0.005), None),
        ],
    },
    "Classic control loops": {
        "description": "PID and PI loops on textbook plants: mass-spring-"
        "damper, DC motor speed and vehicle cruise control.",
        "files": [
            ("mass_spring_damper_params.m", None, None),
            (
                "mass_spring_damper_pid.mdl",
                dict(stop=3, step=0.001),
                "mass_spring_damper_params.m",
            ),
            ("dc_motor_speed_pi.mdl", dict(stop=3, step=0.001), None),
            ("cruise_control_pi.mdl", dict(stop=30, step=0.01), None),
        ],
    },
    "Digital control": {
        "description": "A sampled PI controller with a unit-delay integrator "
        "and seeded sensor noise on a first-order plant.",
        "files": [
            ("digital_pi_first_order.mdl", dict(stop=10, step=0.01), None),
        ],
    },
}


class Client:
    def __init__(self, url, session):
        self.url = url.rstrip("/")
        jar = http.cookiejar.CookieJar()
        if session:
            host = urllib.parse.urlparse(self.url).hostname
            jar.set_cookie(
                http.cookiejar.Cookie(
                    0, "session", session, None, False, host, False, False,
                    "/", True, False, None, False, None, None, {},
                )
            )
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(jar)
        )

    def request(self, method, path, body=None, content_type="application/json"):
        data = None
        headers = {"Origin": self.url}
        if body is not None:
            data = body if isinstance(body, bytes) else json.dumps(body).encode()
            headers["Content-Type"] = content_type
        req = urllib.request.Request(
            self.url + path, data=data, headers=headers, method=method
        )
        try:
            with self.opener.open(req, timeout=120) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as e:
            sys.exit(f"{method} {path}: {e.code} {e.read().decode(errors='replace')}")
        return json.loads(raw) if raw else None

    def dev_login(self):
        # Sets the session cookie, then redirects to the (HTML) home page.
        req = urllib.request.Request(self.url + "/api/auth/dev-login")
        with self.opener.open(req, timeout=30):
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--url", required=True, help="server base URL")
    parser.add_argument("--org", default="Unlinked Examples")
    parser.add_argument("--session", help="session cookie instead of dev login")
    args = parser.parse_args()

    client = Client(args.url, args.session)
    if not args.session:
        client.dev_login()
    if any(o["name"] == args.org for o in client.request("GET", "/api/orgs")):
        sys.exit(f"organization {args.org!r} already exists; pick another --org")
    org = client.request("POST", "/api/orgs", {"name": args.org})
    print(f"organization {org['name']}")

    for name, spec in PROJECTS.items():
        project = client.request(
            "POST",
            f"/api/orgs/{org['id']}/projects",
            {"name": name, "description": spec["description"]},
        )
        print(f"  project {name}")
        uploaded = {}
        for filename, settings, init in spec["files"]:
            query = urllib.parse.urlencode(
                {"path": filename, "message": "Example from the Unlinked repository"}
            )
            info = client.request(
                "POST",
                f"/api/projects/{project['id']}/files?{query}",
                (EXAMPLES / filename).read_bytes(),
                "application/octet-stream",
            )
            uploaded[filename] = info
            print(f"    uploaded {filename}")
        for filename, settings, init in spec["files"]:
            if settings is None:
                continue
            info = uploaded[filename]
            request = {
                "options": {"start": 0, "solver": "rk4", **settings},
                "version": info["latest"]["version"],
            }
            if init:
                script = uploaded[init]
                request["init_script"] = {
                    "file_id": script["id"],
                    "version": script["latest"]["version"],
                }
            result = client.request(
                "POST", f"/api/files/{info['id']}/simulations", request
            )
            run = result["run"]
            detail = f", error: {run['error']}" if run.get("error") else ""
            with_init = f" with {init} v{request['init_script']['version']}" if init else ""
            print(
                f"    ran {filename} v{run['file_version']}: rk4, step {settings['step']} s, "
                f"stop {settings['stop']} s{with_init} -> {run['status']}{detail}"
            )
            if run["status"] != "completed":
                sys.exit(1)


if __name__ == "__main__":
    main()
