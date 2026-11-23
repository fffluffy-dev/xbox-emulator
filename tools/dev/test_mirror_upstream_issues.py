import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mirror_upstream_issues as m

UP = "omonkeyz/xbox-emulator"


def issue(n, title="t", state="open", body="b", user="someone", labels=()):
    return {"number": n, "title": title, "state": state, "body": body, "user": {"login": user},
            "labels": [{"name": l} for l in labels], "created_at": "2026-11-20T10:00:00Z",
            "html_url": "https://github.com/" + UP + "/issues/" + str(n)}


class FakeRun:
    def __init__(self, pages):
        self.pages = list(pages)
        self.calls = []

    def __call__(self, args, capture_output, text):
        self.calls.append(args)
        out = self.pages.pop(0)

        class R:
            returncode = 0
            stdout = json.dumps(out)
            stderr = ""
        return R()


class NeutraliseTests(unittest.TestCase):
    def test_mentions_and_references_become_code(self):
        s = m.neutralise("ping @omonkeyz about #54 and other/repo#3, see https://github.com/omonkeyz/xbox-emulator/pull/12", UP)
        self.assertEqual(s, "ping `@omonkeyz` about `omonkeyz/xbox-emulator#54` and `other/repo#3`, see `omonkeyz/xbox-emulator!12`")

    def test_code_spans_fences_emails_and_headings_are_left_alone(self):
        src = "## 1. Heading\nmail a@b.com, `@x #5`\n```\n@y #6\n```\nafter #7"
        self.assertEqual(m.neutralise(src, UP), "## 1. Heading\nmail a@b.com, `@x #5`\n```\n@y #6\n```\nafter `omonkeyz/xbox-emulator#7`")

    def test_live_links_keep_references(self):
        self.assertEqual(m.neutralise("see #9 and @a", UP, live_links=True), "see omonkeyz/xbox-emulator#9 and `@a`")


class PlanTests(unittest.TestCase):
    def test_existing_mirrors_found_by_marker_or_title(self):
        forks = [
            {"number": 40, "title": "x", "state": "open", "body": "Mirrored from `omonkeyz/xbox-emulator#293` (open)"},
            {"number": 41, "title": "[upstream #54] mesh", "state": "closed", "body": "old"},
            {"number": 42, "title": "unrelated", "state": "open", "body": "Mirrored from `else/where#7`"},
        ]
        found = m.existing_mirrors("f/f", UP, forks)
        self.assertEqual(sorted(found), [54, 293])
        self.assertEqual(found[293]["number"], 40)

    def test_plan_creates_only_new_open_issues_and_syncs_state(self):
        ups = [issue(293, "GTA VI notes"), issue(54, state="closed"), issue(142), issue(7, state="closed")]
        mirrors = {54: {"number": 41, "state": "open"}, 142: {"number": 43, "state": "closed"}}
        p = m.plan(ups, mirrors, UP)
        self.assertEqual([c["upstream"] for c in p["create"]], [293])
        self.assertEqual(p["create"][0]["title"], "[upstream #293] GTA VI notes")
        self.assertEqual(p["close"], [{"upstream": 54, "fork": 41}])
        self.assertEqual(p["reopen"], [{"upstream": 142, "fork": 43}])

    def test_body_carries_the_marker_that_dedupes_it(self):
        b = m.mirror_body(issue(293, body="hi @bob", labels=["gpu"]), UP)
        self.assertEqual(m.existing_mirrors("f/f", UP, [{"number": 1, "title": "x", "state": "open", "body": b}])[293]["number"], 1)
        self.assertIn("`@bob`", b)
        self.assertIn("upstream labels: gpu", b)
        self.assertNotIn("@bob ", b.replace("`@bob`", ""))

    def test_long_bodies_are_cut(self):
        b = m.mirror_body(issue(1, body="x" * 70000), UP)
        self.assertLessEqual(len(b), m.MAX_BODY)


class GhTests(unittest.TestCase):
    def test_search_pages_and_skips_pull_requests(self):
        p1 = {"total_count": 150, "items": [issue(i) for i in range(100)]}
        p1["items"][0]["pull_request"] = {}
        p2 = {"total_count": 150, "items": [issue(i) for i in range(100, 150)]}
        run = FakeRun([p1, p2])
        got = m.search_issues("repo:a/b is:issue", runner=run)
        self.assertEqual(len(got), 149)
        self.assertEqual(len(run.calls), 2)
        self.assertIn("page=2", run.calls[1])

    def test_query_shape(self):
        self.assertEqual(m.upstream_query(UP, "open", "GTA", ["gpu"]), 'repo:' + UP + ' is:issue is:open label:"gpu" GTA')
        self.assertEqual(m.upstream_query(UP, "all", "", []), "repo:" + UP + " is:issue")

    def test_apply_posts_titles_bodies_and_labels(self):
        run = FakeRun([{"number": 50}, {"number": 41}])
        p = {"create": [{"upstream": 293, "title": "T", "body": "B"}], "close": [{"upstream": 54, "fork": 41}], "reopen": [], "kept": []}
        done = m.apply(p, "f/f", ["upstream"], True, runner=run)
        self.assertEqual(done, [("created", 293, 50), ("closed", 54, 41)])
        self.assertIn("labels[]=upstream", run.calls[0])
        self.assertIn("repos/f/f/issues", run.calls[0])
        self.assertIn("state=closed", run.calls[1])


if __name__ == "__main__":
    unittest.main()
