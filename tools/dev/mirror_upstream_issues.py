import argparse
import json
import re
import subprocess
import sys

UPSTREAM = "omonkeyz/xbox-emulator"
FORK = "fffluffy-dev/xbox-emulator"
MARKER = "Mirrored from"
MARKER_RE = re.compile(r"Mirrored from `([\w.-]+/[\w.-]+)#(\d+)`")
TITLE_RE = re.compile(r"^\[upstream #(\d+)\]")
MENTION_RE = re.compile(r"(?<![\w`/.@])@([A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))\b")
REF_RE = re.compile(r"(?<![\w`/&#])#(\d+)\b")
FULL_REF_RE = re.compile(r"(?<![\w`/])([\w.-]+/[\w.-]+)#(\d+)\b")
ISSUE_URL_RE = re.compile(r"https://github\.com/([\w.-]+/[\w.-]+)/(issues|pull)/(\d+)(?:#[\w-]+)?")
FENCE_RE = re.compile(r"^(\s*)(```|~~~)")
MAX_BODY = 60000


def gh_json(args, runner=None):
    run = runner or subprocess.run
    r = run(["gh", "api", *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError("gh api " + " ".join(args) + ": " + (r.stderr or "").strip())
    return json.loads(r.stdout or "null")


def search_issues(query, runner=None, limit=1000):
    out = []
    page = 1
    while len(out) < limit:
        d = gh_json(["-X", "GET", "search/issues", "-f", "q=" + query, "-f", "per_page=100", "-f", "page=" + str(page)], runner)
        items = d.get("items") or []
        out.extend(i for i in items if "pull_request" not in i)
        if len(items) < 100 or page * 100 >= min(1000, d.get("total_count", 0)):
            break
        page += 1
    return out[:limit]


def upstream_query(upstream, state, terms, labels):
    q = ["repo:" + upstream, "is:issue"]
    if state in ("open", "closed"):
        q.append("is:" + state)
    q.extend('label:"' + l + '"' for l in labels)
    if terms:
        q.append(terms)
    return " ".join(q)


def existing_mirrors(fork, upstream, issues):
    found = {}
    for i in issues:
        body = i.get("body") or ""
        n = None
        for repo, num in MARKER_RE.findall(body):
            if repo.lower() == upstream.lower():
                n = int(num)
                break
        if n is None:
            m = TITLE_RE.match(i.get("title") or "")
            n = int(m.group(1)) if m else None
        if n is not None and n not in found:
            found[n] = {"number": i["number"], "state": i.get("state"), "url": i.get("html_url")}
    return found


def code(s):
    return "`" + s + "`"


def neutralise_line(line, upstream, live_links):
    def url(m):
        repo, kind, num = m.group(1), m.group(2), m.group(3)
        return m.group(0) if live_links else code(repo + ("#" if kind == "issues" else "!") + num)

    def full(m):
        return m.group(0) if live_links else code(m.group(1) + "#" + m.group(2))

    def bare(m):
        return code(upstream + "#" + m.group(1)) if not live_links else upstream + "#" + m.group(1)

    parts = re.split(r"(`[^`]*`)", line)
    for k in range(0, len(parts), 2):
        p = parts[k]
        p = ISSUE_URL_RE.sub(url, p)
        p = FULL_REF_RE.sub(full, p)
        p = REF_RE.sub(bare, p)
        p = MENTION_RE.sub(lambda m: code("@" + m.group(1)), p)
        parts[k] = p
    return "".join(parts)


def neutralise(body, upstream, live_links=False):
    out = []
    fence = None
    for line in (body or "").splitlines():
        m = FENCE_RE.match(line)
        if fence is None and m:
            fence = m.group(2)
            out.append(line)
            continue
        if fence is not None:
            if line.strip().startswith(fence):
                fence = None
            out.append(line)
            continue
        out.append(neutralise_line(line, upstream, live_links))
    return "\n".join(out)


def mirror_title(issue):
    return "[upstream #" + str(issue["number"]) + "] " + (issue.get("title") or "").strip()


def mirror_body(issue, upstream, live_links=False):
    n = issue["number"]
    user = (issue.get("user") or {}).get("login") or "unknown"
    labels = ", ".join(l.get("name", "") for l in issue.get("labels") or []) or "none"
    head = [
        MARKER + " " + code(upstream + "#" + str(n)) + " (" + (issue.get("state") or "?") + ", opened "
        + (issue.get("created_at") or "?")[:10] + " by " + code(user) + ", upstream labels: " + labels + ").",
        "",
        "Upstream link: " + code("https://github.com/" + upstream + "/issues/" + str(n)) + ". References and mentions below are "
        "rewritten as code so this copy neither notifies anyone nor adds back-links upstream.",
        "",
        "---",
        "",
    ]
    text = neutralise(issue.get("body") or "_No description upstream._", upstream, live_links)
    room = MAX_BODY - len("\n".join(head)) - 100
    if len(text) > room:
        text = text[:room] + "\n\n_(cut: the upstream description is longer than a GitHub issue body allows)_"
    return "\n".join(head) + "\n" + text + "\n"


def plan(upstream_issues, mirrors, upstream, live_links=False):
    create, close, reopen, kept = [], [], [], []
    for i in sorted(upstream_issues, key=lambda x: x["number"]):
        m = mirrors.get(i["number"])
        if m is None:
            if i.get("state") == "open":
                create.append({"upstream": i["number"], "title": mirror_title(i), "body": mirror_body(i, upstream, live_links)})
            continue
        kept.append({"upstream": i["number"], "fork": m["number"]})
        if i.get("state") == "closed" and m.get("state") == "open":
            close.append({"upstream": i["number"], "fork": m["number"]})
        if i.get("state") == "open" and m.get("state") == "closed":
            reopen.append({"upstream": i["number"], "fork": m["number"]})
    return {"create": create, "close": close, "reopen": reopen, "kept": kept}


def apply(p, fork, labels, sync_state, runner=None):
    done = []
    for c in p["create"]:
        args = ["-X", "POST", "repos/" + fork + "/issues", "-f", "title=" + c["title"], "-f", "body=" + c["body"]]
        for l in labels:
            args += ["-f", "labels[]=" + l]
        r = gh_json(args, runner)
        done.append(("created", c["upstream"], r.get("number")))
    if sync_state:
        for c in p["close"]:
            gh_json(["-X", "PATCH", "repos/" + fork + "/issues/" + str(c["fork"]), "-f", "state=closed", "-f", "state_reason=completed"], runner)
            done.append(("closed", c["upstream"], c["fork"]))
    return done


def main(argv=None):
    ap = argparse.ArgumentParser(description="Copy upstream issues into the fork as mirror issues. Dry run unless --apply.")
    ap.add_argument("--upstream", default=UPSTREAM)
    ap.add_argument("--fork", default=FORK)
    ap.add_argument("--state", choices=["open", "closed", "all"], default="all",
                    help="which upstream issues to read; only open ones are ever created, closed ones drive --sync-state")
    ap.add_argument("--search", default="", help="extra GitHub search terms, for example GTA")
    ap.add_argument("--upstream-label", action="append", default=[], help="only upstream issues with this label")
    ap.add_argument("--label", action="append", default=None, help="label for created mirrors (default: upstream)")
    ap.add_argument("--only", type=int, action="append", default=[], help="only this upstream issue number")
    ap.add_argument("--skip", type=int, action="append", default=[], help="never mirror this upstream issue number")
    ap.add_argument("--limit", type=int, default=1000)
    ap.add_argument("--live-links", action="store_true", help="keep references and links live (adds back-links upstream)")
    ap.add_argument("--sync-state", action="store_true", help="with --apply, close fork mirrors whose upstream issue is closed")
    ap.add_argument("--json", metavar="FILE", help="write the plan as JSON")
    ap.add_argument("--apply", action="store_true", help="create the issues (needs gh auth with write access to the fork)")
    a = ap.parse_args(argv)
    labels = ["upstream"] if a.label is None else [l for l in a.label if l]

    ups = search_issues(upstream_query(a.upstream, a.state, a.search, a.upstream_label), limit=a.limit)
    if a.only:
        ups = [i for i in ups if i["number"] in a.only]
    ups = [i for i in ups if i["number"] not in a.skip]
    forks = search_issues("repo:" + a.fork + ' is:issue "' + MARKER + '" in:body', limit=1000)
    forks += search_issues("repo:" + a.fork + ' is:issue "[upstream" in:title', limit=1000)
    p = plan(ups, existing_mirrors(a.fork, a.upstream, forks), a.upstream, a.live_links)

    print("upstream issues read: " + str(len(ups)) + ", already mirrored: " + str(len(p["kept"])))
    for c in p["create"]:
        print("create  " + a.upstream + "#" + str(c["upstream"]) + "  " + c["title"])
    for c in p["close"]:
        print("close   fork #" + str(c["fork"]) + " (upstream #" + str(c["upstream"]) + " is closed)")
    for c in p["reopen"]:
        print("note    fork #" + str(c["fork"]) + " is closed but upstream #" + str(c["upstream"]) + " is open")
    if a.json:
        with open(a.json, "w", encoding="utf-8") as f:
            json.dump(p, f, ensure_ascii=False, indent=1)
    if not a.apply:
        print("dry run: nothing written (use --apply)")
        return 0
    for what, up, n in apply(p, a.fork, labels, a.sync_state):
        print(what + " fork #" + str(n) + " for upstream #" + str(up))
    return 0


if __name__ == "__main__":
    sys.exit(main())
