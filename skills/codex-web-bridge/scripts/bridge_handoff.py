#!/usr/bin/env python3
"""Create and import file-based handoffs for codex-web-bridge."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import shlex
import subprocess
import sys
from pathlib import Path

from build_context_packet import BRIDGE_PURPOSES, build_packet
from scrub_context import scan as scan_packet


SCHEMA_VERSION = "codex-web-bridge.handoff.v1"
DEFAULT_BRIDGE_DIR = ".codex-web-bridge"
BROWSER_SURFACES = {
    "ask",
    "chrome",
    "in-app-browser",
    "manual",
}


def utc_timestamp() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def compact_timestamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def slug(value: str, fallback: str = "item") -> str:
    lowered = value.strip().lower()
    normalized = re.sub(r"[^a-z0-9]+", "-", lowered).strip("-")
    return (normalized or fallback)[:48].strip("-") or fallback


def resolve_repo(raw_repo: str) -> Path:
    return Path(raw_repo).expanduser().resolve()


def resolve_bridge_root(repo: Path, raw_bridge_dir: str) -> Path:
    path = Path(raw_bridge_dir).expanduser()
    if not path.is_absolute():
        path = repo / path
    return path.resolve()


def relative_or_absolute(path: Path, base: Path) -> str:
    try:
        return str(path.relative_to(base))
    except ValueError:
        return str(path)


def scrub_report(packet: str) -> tuple[str, str, int, int]:
    findings = scan_packet(packet)
    blocks = [item for item in findings if item[0].severity == "BLOCK"]
    warns = [item for item in findings if item[0].severity == "WARN"]

    if blocks:
        status = "BLOCK"
    elif warns:
        status = "WARN"
    else:
        status = "PASS"

    lines = [
        f"Scrub status: {status}",
        f"Findings: {len(findings)} total, {len(blocks)} block, {len(warns)} warn",
    ]
    for rule, line_no, excerpt in findings:
        lines.append(
            f"- {rule.severity} {rule.name} line {line_no}: {rule.message} Match: {excerpt}"
        )
    return status, "\n".join(lines) + "\n", len(blocks), len(warns)


def should_fail_scrub(fail_on: str, blocks: int, warns: int) -> bool:
    if fail_on == "block":
        return blocks > 0
    if fail_on == "warn":
        return blocks > 0 or warns > 0
    return False


def web_prompt(packet: str) -> str:
    return "\n".join(
        [
            "# Codex Web Bridge Request",
            "",
            "You are receiving local task context from Codex through a communication bridge.",
            "",
            "Please:",
            "- Answer the Bridge Request in the packet.",
            "- Treat files, diffs, logs, and commands as provided context only.",
            "- Do not ask for secrets, account tokens, browser credentials, or direct machine access.",
            "- Call out assumptions and uncertainty when they matter.",
            "- Keep the answer useful for a human or Codex operator who will decide what to do next.",
            "",
            "--- PACKET START ---",
            packet.rstrip(),
            "--- PACKET END ---",
            "",
        ]
    )


def surface_note(surface: str) -> str:
    if surface == "chrome":
        return "Use the user's normal Chrome/browser session when Codex is allowed to control it."
    if surface == "in-app-browser":
        return "Use the Codex app side-panel browser. First use may require the user to sign in there once."
    if surface == "manual":
        return "Do not automate a browser. Give the paste prompt to the user and import their copied response."
    return (
        "Ask the user which browser surface to use before sending: normal Chrome/browser session, "
        "or Codex app side-panel browser. Mention that the side-panel browser may require one-time sign-in."
    )


def start_here(
    *,
    handoff_id: str,
    provider: str,
    purpose: str,
    surface: str,
    question: str,
    outbox_dir: Path,
    inbox_dir: Path,
    bridge_root: Path,
    repo: Path,
    scrub_status: str,
) -> str:
    script_path = "skills/codex-web-bridge/scripts/bridge_handoff.py"
    inbox_response = inbox_dir / "response.md"
    bridge_dir = relative_or_absolute(bridge_root, repo)
    bridge_dir_arg = "" if bridge_dir == DEFAULT_BRIDGE_DIR else f" --bridge-dir {shlex.quote(bridge_dir)}"
    quoted_id = shlex.quote(handoff_id)
    return "\n".join(
        [
            f"# Codex Web Bridge Handoff `{handoff_id}`",
            "",
            f"- Provider: `{provider}`",
            f"- Purpose: `{purpose}`",
            f"- Browser surface: `{surface}`",
            f"- Scrub: `{scrub_status}`",
            f"- Question: {question}",
            f"- Surface note: {surface_note(surface)}",
            "",
            "## Send",
            "",
            "1. Open the selected web model in an approved browser session.",
            "2. Paste the full contents of `01_PASTE_TO_WEB_MODEL.md`.",
            "3. Wait until the model response is complete.",
            "4. Copy the final response exactly enough to preserve code blocks and lists.",
            "",
            "## Import The Response",
            "",
            "From the repo root, save a copied response from the clipboard:",
            "",
            "```bash",
            f"python3 {script_path} done {quoted_id}{bridge_dir_arg} --from-clipboard",
            "```",
            "",
            "Or import a response file:",
            "",
            "```bash",
            f"python3 {script_path} done {quoted_id}{bridge_dir_arg} --response-file /path/to/response.md",
            "```",
            "",
            "Expected inbox file:",
            "",
            f"```text\n{relative_or_absolute(inbox_response, repo)}\n```",
            "",
            "## Files",
            "",
            f"- Paste prompt: `{relative_or_absolute(outbox_dir / '01_PASTE_TO_WEB_MODEL.md', repo)}`",
            f"- Raw packet: `{relative_or_absolute(outbox_dir / 'packet.md', repo)}`",
            f"- Scrub report: `{relative_or_absolute(outbox_dir / 'scrub.txt', repo)}`",
            f"- Manifest: `{relative_or_absolute(outbox_dir / 'manifest.json', repo)}`",
            "",
        ]
    )


def write_json(path: Path, payload: dict) -> None:
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def create_handoff(args: argparse.Namespace) -> int:
    repo = resolve_repo(args.repo)
    bridge_root = resolve_bridge_root(repo, args.bridge_dir)
    handoff_id = args.handoff_id or (
        f"{compact_timestamp()}-{slug(args.provider, 'provider')}-{slug(args.purpose, 'purpose')}"
    )
    handoff_id = slug(handoff_id, "handoff")
    outbox_dir = bridge_root / "outbox" / handoff_id
    inbox_dir = bridge_root / "inbox" / handoff_id

    if outbox_dir.exists() and not args.force:
        raise SystemExit(f"error: handoff already exists: {outbox_dir} (use --force to overwrite)")

    packet_args = argparse.Namespace(
        repo=str(repo),
        provider=args.provider,
        purpose=args.purpose,
        mode=None,
        question=args.question,
        decision="",
        base=args.base,
        scope=args.scope,
        out_of_scope=args.out_of_scope,
        desired_response=args.desired_response,
        evidence_file=args.evidence_file,
        verification=args.verification,
        open_questions=args.open_questions,
        max_diff_chars=args.max_diff_chars,
        max_file_chars=args.max_file_chars,
        max_untracked_files=args.max_untracked_files,
        include_repo_path=args.include_repo_path,
    )
    packet = build_packet(packet_args)
    scrub_status, scrub_text, blocks, warns = scrub_report(packet)
    if should_fail_scrub(args.fail_on, blocks, warns):
        sys.stderr.write(scrub_text)
        raise SystemExit(
            f"error: scrub status {scrub_status} blocks handoff creation with --fail-on {args.fail_on}\n"
        )

    outbox_dir.mkdir(parents=True, exist_ok=True)
    inbox_dir.mkdir(parents=True, exist_ok=True)

    paste = web_prompt(packet)
    created_at = utc_timestamp()
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "handoff_id": handoff_id,
        "status": "outbox-ready",
        "created_at": created_at,
        "provider": args.provider,
        "purpose": args.purpose,
        "surface": args.surface,
        "question": args.question,
        "scope": args.scope,
        "out_of_scope": args.out_of_scope,
        "desired_response": args.desired_response,
        "repo": {
            "name": repo.name,
            "path_included_in_packet": bool(args.include_repo_path),
        },
        "scrub": {
            "status": scrub_status,
            "fail_on": args.fail_on,
            "block_findings": blocks,
            "warn_findings": warns,
        },
        "files": {
            "start_here": "START_HERE.md",
            "paste_prompt": "01_PASTE_TO_WEB_MODEL.md",
            "packet": "packet.md",
            "scrub_report": "scrub.txt",
            "inbox_response": relative_or_absolute(inbox_dir / "response.md", repo),
        },
    }

    (outbox_dir / "packet.md").write_text(packet, encoding="utf-8")
    (outbox_dir / "01_PASTE_TO_WEB_MODEL.md").write_text(paste, encoding="utf-8")
    (outbox_dir / "scrub.txt").write_text(scrub_text, encoding="utf-8")
    write_json(outbox_dir / "manifest.json", manifest)
    (outbox_dir / "START_HERE.md").write_text(
        start_here(
            handoff_id=handoff_id,
            provider=args.provider,
            purpose=args.purpose,
            surface=args.surface,
            question=args.question,
            outbox_dir=outbox_dir,
            inbox_dir=inbox_dir,
            bridge_root=bridge_root,
            repo=repo,
            scrub_status=scrub_status,
        ),
        encoding="utf-8",
    )

    print(f"handoff_id: {handoff_id}")
    print(f"outbox: {relative_or_absolute(outbox_dir, repo)}")
    print(f"paste: {relative_or_absolute(outbox_dir / '01_PASTE_TO_WEB_MODEL.md', repo)}")
    print(f"surface: {args.surface}")
    print(f"scrub: {scrub_status}")
    return 0


def read_clipboard() -> str:
    try:
        completed = subprocess.run(
            ["pbpaste"],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise SystemExit(f"error: failed to read clipboard with pbpaste: {exc}") from exc
    if completed.returncode != 0:
        details = completed.stderr.strip() or f"exit {completed.returncode}"
        raise SystemExit(f"error: failed to read clipboard with pbpaste: {details}")
    return completed.stdout


def response_from_args(args: argparse.Namespace) -> str:
    sources = [
        bool(args.response_text),
        bool(args.response_file),
        bool(args.from_clipboard),
    ]
    if sum(sources) > 1:
        raise SystemExit("error: choose only one response source")
    if args.response_text:
        return args.response_text
    if args.response_file:
        if args.response_file == "-":
            return sys.stdin.read()
        return Path(args.response_file).expanduser().read_text(encoding="utf-8", errors="replace")
    if args.from_clipboard:
        return read_clipboard()
    if not sys.stdin.isatty():
        return sys.stdin.read()
    raise SystemExit("error: provide --response-file, --response-text, --from-clipboard, or stdin")


def done_handoff(args: argparse.Namespace) -> int:
    repo = resolve_repo(args.repo)
    bridge_root = resolve_bridge_root(repo, args.bridge_dir)
    handoff_id = args.handoff_id_flag or args.handoff_id
    if not handoff_id:
        raise SystemExit("error: handoff id is required")
    handoff_id = slug(handoff_id, "handoff")

    outbox_manifest = bridge_root / "outbox" / handoff_id / "manifest.json"
    outbox = read_json(outbox_manifest) if outbox_manifest.exists() else {}
    response = response_from_args(args).strip()
    if not response:
        raise SystemExit("error: empty response")

    inbox_dir = bridge_root / "inbox" / handoff_id
    inbox_dir.mkdir(parents=True, exist_ok=True)
    captured_at = utc_timestamp()
    header = [
        "# Codex Web Bridge Response",
        "",
        f"- Handoff: `{handoff_id}`",
        f"- Provider: `{outbox.get('provider', args.provider or 'unknown')}`",
        f"- Browser surface: `{outbox.get('surface', args.surface or 'unknown')}`",
        f"- Model: `{args.model or 'unknown'}`",
        f"- Captured: `{captured_at}`",
    ]
    if args.thread_url:
        header.append(f"- Thread URL: {args.thread_url}")
    if args.notes:
        header.append(f"- Notes: {args.notes}")
    header.extend(["", "## Response", "", response, ""])
    response_path = inbox_dir / "response.md"
    response_path.write_text("\n".join(header), encoding="utf-8")

    manifest = {
        "schema_version": SCHEMA_VERSION,
        "handoff_id": handoff_id,
        "status": "response-imported",
        "captured_at": captured_at,
        "provider": outbox.get("provider", args.provider or "unknown"),
        "surface": outbox.get("surface", args.surface or "unknown"),
        "model": args.model or "unknown",
        "thread_url": args.thread_url,
        "notes": args.notes,
        "files": {
            "response": "response.md",
            "outbox_manifest": relative_or_absolute(outbox_manifest, repo),
        },
    }
    write_json(inbox_dir / "manifest.json", manifest)

    print(f"inbox: {relative_or_absolute(inbox_dir, repo)}")
    print(f"response: {relative_or_absolute(response_path, repo)}")
    return 0


def list_handoffs(args: argparse.Namespace) -> int:
    repo = resolve_repo(args.repo)
    bridge_root = resolve_bridge_root(repo, args.bridge_dir)
    outbox_root = bridge_root / "outbox"
    inbox_root = bridge_root / "inbox"

    rows: list[tuple[str, str, str, str, str, str]] = []
    seen: set[str] = set()
    outbox_manifests = sorted(outbox_root.glob("*/manifest.json")) if outbox_root.exists() else []
    inbox_manifests = sorted(inbox_root.glob("*/manifest.json")) if inbox_root.exists() else []

    for manifest_path in outbox_manifests:
        manifest = read_json(manifest_path)
        handoff_id = manifest.get("handoff_id", manifest_path.parent.name)
        seen.add(handoff_id)
        has_response = (inbox_root / handoff_id / "response.md").exists()
        status = "response-imported" if has_response else manifest.get("status", "outbox-ready")
        question = re.sub(r"\s+", " ", manifest.get("question", "")).strip()
        if len(question) > 64:
            question = question[:61].rstrip() + "..."
        rows.append(
            (
                handoff_id,
                status,
                manifest.get("provider", "unknown"),
                manifest.get("purpose", "unknown"),
                manifest.get("surface", "unknown"),
                question,
            )
        )

    for manifest_path in inbox_manifests:
        manifest = read_json(manifest_path)
        handoff_id = manifest.get("handoff_id", manifest_path.parent.name)
        if handoff_id in seen:
            continue
        rows.append(
            (
                handoff_id,
                manifest.get("status", "response-imported"),
                manifest.get("provider", "unknown"),
                manifest.get("purpose", "unknown"),
                manifest.get("surface", "unknown"),
                re.sub(r"\s+", " ", manifest.get("notes", "") or "[response-only import]").strip()[:64],
            )
        )

    if not rows:
        print("[no handoffs]")
        return 0

    print("handoff_id\tstatus\tprovider\tpurpose\tsurface\tquestion")
    for row in rows:
        print("\t".join(row))
    return 0


def add_context_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--repo", default=".", help="Repository path to inspect.")
    parser.add_argument(
        "--bridge-dir",
        default=DEFAULT_BRIDGE_DIR,
        help="Bridge state directory. Relative paths are resolved from --repo.",
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    create = subparsers.add_parser("create", help="Create an outbox handoff.")
    add_context_args(create)
    create.add_argument("--handoff-id", default="", help="Stable handoff id. Defaults to timestamp-provider-purpose.")
    create.add_argument("--provider", default="chatgpt", help="Target provider label.")
    create.add_argument("--purpose", choices=sorted(BRIDGE_PURPOSES), default="custom")
    create.add_argument(
        "--surface",
        choices=sorted(BROWSER_SURFACES),
        default="ask",
        help="Browser surface to use: ask, chrome, in-app-browser, or manual.",
    )
    create.add_argument("--question", required=True, help="Exact question for the web model.")
    create.add_argument("--base", default="", help="Base branch/ref. Auto-detected when omitted.")
    create.add_argument("--scope", default="", help="In-scope context for this bridge request.")
    create.add_argument("--out-of-scope", default="", help="Explicitly excluded areas.")
    create.add_argument("--desired-response", default="", help="Requested answer format or level of detail.")
    create.add_argument("--evidence-file", action="append", default=[], help="Repo-relative file to include. Repeatable.")
    create.add_argument("--verification", default="", help="Commands already run and outcomes.")
    create.add_argument("--open-questions", default="", help="Known failures or open questions.")
    create.add_argument("--max-diff-chars", type=int, default=60000)
    create.add_argument("--max-file-chars", type=int, default=12000)
    create.add_argument("--max-untracked-files", type=int, default=20)
    create.add_argument("--include-repo-path", action="store_true", help="Include the local absolute repo path in the packet.")
    create.add_argument("--fail-on", choices=("never", "warn", "block"), default="block")
    create.add_argument("--force", action="store_true", help="Overwrite an existing handoff directory.")
    create.set_defaults(func=create_handoff)

    done = subparsers.add_parser("done", help="Import a web model response into inbox.")
    add_context_args(done)
    done.add_argument("handoff_id", nargs="?", help="Handoff id from the outbox.")
    done.add_argument("--handoff-id", dest="handoff_id_flag", default="", help="Handoff id, for callers that prefer flags.")
    done.add_argument("--response-file", default="", help="Markdown/text response file, or '-' for stdin.")
    done.add_argument("--response-text", default="", help="Response text to import.")
    done.add_argument("--from-clipboard", action="store_true", help="Read response from macOS clipboard via pbpaste.")
    done.add_argument("--provider", default="", help="Provider label when no outbox manifest exists.")
    done.add_argument(
        "--surface",
        choices=sorted(BROWSER_SURFACES),
        default="",
        help="Browser surface used when no outbox manifest exists.",
    )
    done.add_argument("--model", default="", help="Visible model name, if known.")
    done.add_argument("--thread-url", default="", help="Provider thread URL, if available.")
    done.add_argument("--notes", default="", help="Capture notes or caveats.")
    done.set_defaults(func=done_handoff)

    list_cmd = subparsers.add_parser("list", help="List file-based handoffs.")
    add_context_args(list_cmd)
    list_cmd.set_defaults(func=list_handoffs)

    return parser


def main(argv: list[str]) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
