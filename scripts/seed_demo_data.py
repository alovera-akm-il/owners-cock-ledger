#!/usr/bin/env python3
"""Populates a fresh instance of the app with realistic demo data, via the
real HTTP API — the same way a browser would, not a DB shortcut — so every
page has content that looks like the mockups (mockups/) instead of an empty
state. Boots its own server subprocess against a scratch DATA_DIR.

Usage:
    python3 scripts/seed_demo_data.py [--data-dir PATH] [--reset] [--keep-running]

Requires: `requests` (pip install requests), and a built binary at
target/release/owners-cock-ledger or target/debug/owners-cock-ledger
(run `cargo build --release` first for the fast path).

What it creates:
  - One keyholder ("MK") with a filled-in profile.
  - Three submissives (Riley, Sam, Jordan — the names the mockups
    themselves use) with filled-in profiles, at different states:
      Riley: locked, ~6 days into confinement with a scheduled release,
             an unacknowledged safety alert, a pending proof review,
             a completed play session, rated limits, points, redemptions.
      Sam:   unlocked, a missed check-in, an assigned punishment.
      Jordan: locked, freshly linked, lighter activity.
  - A shared catalog: tasks (with proof requirements and on-success/
    on-failure chaining), rewards (points-redeemable), punishments
    (time-extension), check-in templates, a play-session template, and
    a recurring task rule.
  - Devices, toys (one with a photo), confinement sessions, proof
    submissions in multiple review states, check-ins, a full play-session
    lifecycle (scheduled -> started -> ended -> judged), points
    adjustments and a pending redemption request, and limit ratings.

At the end it prints every login and leaves the server running (unless
--keep-running is omitted) so you can open a browser at the printed URL
immediately.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

try:
    import requests
except ImportError:
    sys.exit("This script needs the `requests` package: pip install requests")

REPO_ROOT = Path(__file__).resolve().parent.parent

# A minimal valid 1x1 PNG — the exact same fixture bytes as TINY_PNG in
# src/main.rs's test suite, since the upload endpoints actually decode the
# image server-side (via the `image` crate) and reject anything malformed.
TINY_PNG = bytes([
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
    0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
    0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
    0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
    0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
])

KEYHOLDER_EMAIL = "keyholder@demo.test"
SUBMISSIVES = [
    {"name": "Riley", "email": "riley@demo.test", "password": "RileyDemoPass1!"},
    {"name": "Sam", "email": "sam@demo.test", "password": "SamDemoPass1!"},
    {"name": "Jordan", "email": "jordan@demo.test", "password": "JordanDemoPass1!"},
]


def find_binary() -> Path:
    for candidate in ("target/release/owners-cock-ledger", "target/debug/owners-cock-ledger"):
        p = REPO_ROOT / candidate
        if p.exists():
            return p
    sys.exit("No built binary found — run `cargo build --release` (or `cargo build`) first.")


class ApiError(RuntimeError):
    pass


class Client:
    """One actor's session (keyholder or a submissive) — handles the
    double-submit CSRF cookie the same way the real frontend JS does."""

    def __init__(self, base_url: str):
        self.base_url = base_url
        self.session = requests.Session()

    def _csrf_token(self) -> str:
        return self.session.cookies.get("ocl_csrf", "")

    def prime(self):
        """A plain GET issues the CSRF cookie if this session doesn't have one yet."""
        self.session.get(self.base_url + "/health")

    def call(self, method: str, path: str, json_body=None, files=None, data=None, expect=None):
        self.prime()
        headers = {"X-CSRF-Token": self._csrf_token()}
        resp = self.session.request(
            method, self.base_url + path, json=json_body, files=files, data=data, headers=headers
        )
        if expect is not None and resp.status_code not in expect:
            raise ApiError(
                f"{method} {path} -> {resp.status_code} (expected {expect}): {resp.text[:500]}"
            )
        if resp.status_code >= 400 and expect is None:
            raise ApiError(f"{method} {path} -> {resp.status_code}: {resp.text[:500]}")
        return resp

    def get(self, path, **kw):
        return self.call("GET", path, **kw)

    def post(self, path, json_body=None, **kw):
        return self.call("POST", path, json_body=json_body, **kw)

    def post_multipart(self, path, fields: dict, file_field=None, file_tuple=None, **kw):
        """Text fields alone (no attachment) still need a real
        multipart/form-data body — the Multipart extractor server-side
        doesn't accept application/x-www-form-urlencoded, which is what
        `requests` sends if you pass plain `data=` with no `files=`. So
        every text field is sent through `files=` too, using the
        (None, value) tuple form that forces multipart encoding either way.
        """
        parts = {k: (None, str(v)) for k, v in fields.items() if v is not None}
        if file_field:
            parts[file_field] = file_tuple
        return self.call("POST", path, files=parts, **kw)

    def patch(self, path, json_body=None, **kw):
        return self.call("PATCH", path, json_body=json_body, **kw)

    def put(self, path, json_body=None, **kw):
        return self.call("PUT", path, json_body=json_body, **kw)

    def login(self, email: str, password: str):
        r = self.post("/api/v1/auth/login", json_body={"email": email, "password": password})
        return r.json()


def step(label: str):
    print(f"  -> {label}")


def wait_for_health(base_url: str, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if requests.get(base_url + "/health", timeout=1).status_code == 200:
                return
        except requests.RequestException:
            pass
        time.sleep(0.3)
    sys.exit(f"Server never became healthy at {base_url}")


def bootstrap_keyholder(binary: Path, data_dir: Path) -> str:
    """`admin create-keyholder` only runs against a stopped/not-yet-running
    DB (it opens its own short-lived connection) — safe to run before the
    server starts."""
    env = {**os.environ, "DATA_DIR": str(data_dir)}
    result = subprocess.run(
        [str(binary), "admin", "create-keyholder", "--display-name", "MK", "--yes", KEYHOLDER_EMAIL],
        env=env, capture_output=True, text=True, check=True,
    )
    for line in result.stdout.splitlines():
        if "Temporary password" in line:
            return line.split(":", 1)[1].strip()
    raise RuntimeError("Could not find the temporary password in admin output:\n" + result.stdout)


def start_server(binary: Path, data_dir: Path, listen_addr: str) -> subprocess.Popen:
    env = {
        **os.environ,
        "DATA_DIR": str(data_dir),
        "LISTEN_ADDR": listen_addr,
        "INSECURE_COOKIES": "1",  # this script talks plain HTTP to 127.0.0.1
    }
    proc = subprocess.Popen(
        [str(binary)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    return proc


def seed(base_url: str, kh_password: str):
    kh = Client(base_url)
    kh.login(KEYHOLDER_EMAIL, kh_password)
    step("Keyholder logged in")

    kh.patch("/api/v1/profile", json_body={
        "bio": "Been doing this for a few years — I run a fairly structured house.",
        "contact_info": "Signal: mk.demo",
        "hard_limits": "no permanent marks, no breath play without a spotter",
        "soft_limits": "ask first on anything public-facing",
        "timezone": "America/New_York",
    })
    step("Keyholder profile filled in")

    # ---------- Catalog templates ----------
    def new_template(body):
        return kh.post("/api/v1/keyholder/templates", json_body=body).json()["id"]

    punishment_extra_day = new_template({
        "kind": "punishment", "title": "Extra day locked",
        "description": "Straightforward time penalty for a missed obligation.",
        "effect_kind": "time_extension", "time_extension_seconds": 86400,
    })
    reward_movie_night = new_template({
        "kind": "reward", "title": "Choose tonight's movie",
        "description": "Full control of the remote, no complaints.",
        "effect_kind": "grant",
    })
    reward_time_off = new_template({
        "kind": "reward", "title": "6 hours off the timer",
        "effect_kind": "time_reduction", "time_reduction_seconds": 6 * 3600,
        "points_cost": 30,
    })
    task_cold_shower = new_template({
        "kind": "task", "title": "cold shower, video required",
        "description": "Full minute, video proof, no cutting away.",
        "completion_type": "proof_required", "proof_media_types": ["video"],
        "default_deadline_seconds": 3 * 3600,
        "on_failure_template_id": punishment_extra_day,
        "points_delta": 5,
    })
    task_lines = new_template({
        "kind": "task", "title": "500 lines, handwritten",
        "description": "\"I will submit proof on time.\" x500, photographed.",
        "completion_type": "acknowledge_only",
        "default_deadline_seconds": 6 * 3600,
    })
    task_evening_checkin = new_template({
        "kind": "task", "title": "evening check-in",
        "description": "Photo, code visible, every night.",
        "completion_type": "proof_required", "proof_media_types": ["photo"],
        "default_deadline_seconds": 12 * 3600,
        "on_success_template_id": reward_movie_night,
    })
    step(f"Catalog: {5} templates created (2 punishments/rewards, 3 tasks)")

    checkin_template = kh.post("/api/v1/keyholder/checkin-templates", json_body={
        "title": "Morning cage check-in",
        "auto_escalate_on_red": True,
        "fields": [
            {"field_key": "skin_status", "label": "Skin status", "field_type": "select",
             "config": {"options": ["normal", "chafing", "irritated"]}, "required": True},
            {"field_key": "comfort", "label": "Comfort (1-10)", "field_type": "scale",
             "config": {"min": 1, "max": 10}, "required": True},
            {"field_key": "notes", "label": "Notes", "field_type": "text",
             "config": {}, "required": False},
        ],
    }).json()["id"]
    step("Check-in template created")

    play_template = kh.post("/api/v1/keyholder/play-session-templates", json_body={
        "title": "Standard scene",
        "setup_notes": "Usual gear, safeword posted where both can see it.",
        "suggested_toy_categories": ["impact", "restraint"],
        "planned_duration_seconds": 3600,
    }).json()["id"]
    step("Play-session template created")

    # ---------- Submissives ----------
    subs = {}
    for person in SUBMISSIVES:
        invite = kh.post("/api/v1/keyholder/invites", json_body={"expires_in_hours": 48}).json()
        sub_client = Client(base_url)
        sub_client.prime()
        sub_client.post("/api/v1/auth/invites/redeem", json_body={
            "token": invite["token"], "email": person["email"],
            "password": person["password"], "display_name": person["name"],
        })
        subs[person["name"]] = sub_client
        step(f"{person['name']} invited and account created")

    roster = kh.get("/api/v1/keyholder/submissives").json()
    ids = {row["display_name"]: row["submissive_id"] for row in roster}

    profiles = {
        "Riley": {"bio": "Started this journey about 6 months ago.", "safeword": "pineapple",
                   "emergency_contact": "Jamie, sister, 555-0100",
                   "hard_limits": "no permanent marks", "soft_limits": "ask first",
                   "timezone": "America/New_York"},
        "Sam": {"bio": "New to structured play, still figuring out the rhythm.",
                 "safeword": "yellow/red", "emergency_contact": "on file with Keyholder",
                 "hard_limits": "no breath play", "soft_limits": "impact only with warmup",
                 "timezone": "America/Chicago"},
        "Jordan": {"bio": "Long-distance dynamic, mostly text check-ins.",
                    "safeword": "banana", "emergency_contact": "roommate, see notes",
                    "hard_limits": "no public exposure", "soft_limits": "fine with most restraint",
                    "timezone": "America/Los_Angeles"},
    }
    for name, sub in subs.items():
        sub.patch("/api/v1/profile", json_body=profiles[name])
    step("Submissive profiles filled in")

    for name, sub_id in ids.items():
        kh.patch(f"/api/v1/keyholder/submissives/{sub_id}/link/settings", json_body={
            "points_enabled": True, "self_report_allowed": name != "Riley",
            "catalog_visible_to_submissive": True,
        })
    step("Points enabled on every link")

    # ---------- Devices, toys, confinement ----------
    now = int(time.time())
    device_ids, toy_ids = {}, {}
    for name in ids:
        device_ids[name] = kh.post(
            f"/api/v1/keyholder/submissives/{ids[name]}/devices",
            json_body={"name": f"{name.lower()}-steel-01", "description": "daily wear"},
        ).json()["id"]
        toy_ids[name] = kh.post(
            f"/api/v1/keyholder/submissives/{ids[name]}/toys",
            json_body={
                "name": "steel cage", "category": "chastity", "material": "steel",
                "brand": "CB-X", "compatible_device_id": device_ids[name],
                "tags": ["daily"], "acquired_at": "2025-01-01T00:00:00Z",
            },
        ).json()["id"]
    kh.call("POST", f"/api/v1/toys/{toy_ids['Riley']}/photo",
             files={"photo": ("toy.png", TINY_PNG, "image/png")})
    step("Devices and toys created (one photo uploaded)")

    kh.post(f"/api/v1/keyholder/submissives/{ids['Riley']}/confinement-sessions", json_body={
        "device_id": device_ids["Riley"], "started_reason": "scheduled",
        "target_release_at": now + 6 * 86400 + 4 * 3600,
        "notes": "usual monthly cycle",
    })
    kh.post(f"/api/v1/keyholder/submissives/{ids['Jordan']}/confinement-sessions", json_body={
        "device_id": device_ids["Jordan"], "started_reason": "voluntary",
        "target_release_at": now + 1 * 86400 + 2 * 3600,
    })
    step("Confinement sessions started (Riley, Jordan locked; Sam left unlocked)")

    # ---------- Assignments ----------
    kh.post(f"/api/v1/keyholder/submissives/{ids['Riley']}/assignments", json_body={
        "template_id": task_evening_checkin, "deadline_at": None,
    })
    kh.post(f"/api/v1/keyholder/submissives/{ids['Riley']}/assignments", json_body={
        "template_id": task_cold_shower,
    })
    kh.post(f"/api/v1/keyholder/submissives/{ids['Sam']}/assignments", json_body={
        "template_id": task_lines,
    })
    kh.post(f"/api/v1/keyholder/submissives/{ids['Sam']}/assignments", json_body={
        "kind": "punishment", "title": "no orgasm for 3 extra days",
        "effect_kind": "time_extension", "time_extension_seconds": 3 * 86400,
        "notes": "escalated after a missed deadline",
    })
    step("Tasks/punishments assigned")

    # ---------- Proof submissions ----------
    riley_note = subs["Riley"].post_multipart(
        "/api/v1/submissive/proof-submissions",
        fields={"kind": "note", "metadata": json.dumps({"notes": "checking in, all good"})},
    ).json()
    subs["Riley"].post_multipart(
        "/api/v1/submissive/proof-submissions",
        fields={"kind": "photo"},
        file_field="files", file_tuple=("proof.png", TINY_PNG, "image/png"),
    )
    kh.post(f"/api/v1/keyholder/proof-submissions/{riley_note['id']}/review", json_body={
        "status": "verified", "review_notes": "all good",
    })
    subs["Sam"].post_multipart(
        "/api/v1/submissive/proof-submissions",
        fields={"kind": "photo"},
        file_field="files", file_tuple=("proof.png", TINY_PNG, "image/png"),
    )
    step("Proof submitted (mixed pending/verified) across submissives")

    # ---------- Limit ratings ----------
    items = subs["Riley"].get("/api/v1/submissive/limit-items").json()
    ratings = ["hard", "soft", "okay"]
    for i, item in enumerate(items[:6]):
        for name in ("Riley", "Sam", "Jordan"):
            subs[name].put(f"/api/v1/submissive/limit-ratings/{item['id']}", json_body={
                "rating": ratings[i % 3],
                "notes": "only if we've talked about it" if ratings[i % 3] == "soft" else None,
            })
    step("Limit items rated for every submissive")

    # ---------- Safety alert ----------
    subs["Riley"].post("/api/v1/submissive/safety-alert", json_body={
        "message": "Device feels too tight, need to check",
    })
    step("Safety alert raised (Riley)")

    # ---------- Points and redemption ----------
    kh.post(f"/api/v1/keyholder/submissives/{ids['Riley']}/points/adjust", json_body={
        "delta": 50, "notes": "bonus for a clean week",
    })
    subs["Riley"].post(f"/api/v1/submissive/rewards/{reward_time_off}/redeem")
    step("Points granted and a redemption request raised (Riley)")

    # ---------- Recurring task ----------
    kh.post(f"/api/v1/keyholder/submissives/{ids['Riley']}/recurring-tasks", json_body={
        "template_id": task_lines, "recurrence_kind": "interval_hours",
        "recurrence_value": {"hours": 48}, "allow_overlap": False,
    })
    step("Recurring task rule created (Riley)")

    # ---------- Check-ins ----------
    for name in ("Riley", "Jordan"):
        subs[name].post("/api/v1/submissive/checkins", json_body={
            "template_id": checkin_template, "color": "green",
            "field_values": {"skin_status": "normal", "comfort": 8, "notes": "all fine"},
        })
    step("Check-ins submitted")

    # ---------- Full play-session lifecycle (Riley) ----------
    session = kh.post(f"/api/v1/keyholder/submissives/{ids['Riley']}/play-sessions", json_body={
        "template_id": play_template, "toy_ids": [toy_ids["Riley"]],
    }).json()
    sid = session["id"]
    kh.post(f"/api/v1/keyholder/play-sessions/{sid}/start", json_body={})
    kh.post(f"/api/v1/keyholder/play-sessions/{sid}/end", json_body={"safety_check_ok": True})
    kh.patch(f"/api/v1/keyholder/play-sessions/{sid}/judgement", json_body={
        "judgement_notes": "Went great, stayed responsive the whole time.",
        "reward": {"title": "Extra praise", "effect_kind": "grant"},
    })
    kh.patch(f"/api/v1/keyholder/play-sessions/{sid}/complete", json_body={})
    step("Play session run end-to-end and judged (Riley)")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data-dir", default=str(REPO_ROOT / ".demo-data"))
    parser.add_argument("--listen-addr", default="127.0.0.1:8199")
    parser.add_argument("--reset", action="store_true", help="wipe --data-dir before seeding")
    parser.add_argument("--keep-running", action="store_true", default=True,
                         help="leave the seeded server running after this script exits (default)")
    parser.add_argument("--stop-after", action="store_true",
                         help="stop the server once seeding finishes instead of leaving it running")
    args = parser.parse_args()

    binary = find_binary()
    data_dir = Path(args.data_dir).resolve()

    if args.reset and data_dir.exists():
        shutil.rmtree(data_dir)
    data_dir.mkdir(parents=True, exist_ok=True)

    if any(data_dir.iterdir()):
        sys.exit(
            f"{data_dir} isn't empty. Pass --reset to wipe it, or point --data-dir "
            "somewhere fresh — this script is meant to run once against a clean DB."
        )

    print(f"Bootstrapping keyholder account in {data_dir} ...")
    kh_password = bootstrap_keyholder(binary, data_dir)

    base_url = f"http://{args.listen_addr}"
    print(f"Starting server at {base_url} ...")
    proc = start_server(binary, data_dir, args.listen_addr)
    try:
        wait_for_health(base_url)
        print("Seeding demo data:")
        seed(base_url, kh_password)
    except Exception:
        proc.terminate()
        raise

    print("\n" + "=" * 60)
    print(f"Done. Server running at {base_url}")
    print("=" * 60)
    print(f"Keyholder: {KEYHOLDER_EMAIL} / {kh_password}")
    for person in SUBMISSIVES:
        print(f"Submissive ({person['name']}): {person['email']} / {person['password']}")
    print("=" * 60)

    if args.stop_after:
        proc.terminate()
        proc.wait(timeout=5)
        print("Server stopped (--stop-after was set).")
    else:
        print(f"Server left running as PID {proc.pid} — stop it with: kill {proc.pid}")


if __name__ == "__main__":
    main()
