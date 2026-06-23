#!/usr/bin/env python3
"""bt.py — translate between the `.bt` behaviour-tree form and TOML, both ways.

Round-trips semantically (run / env / behaviours / root), not byte-for-byte:
comments and whitespace are not preserved, and anonymous inline behaviours are
canonicalised (see below).

Usage:
    bt.py foo.bt              # infer from extension: .bt  -> TOML on stdout
    bt.py foo.toml           # infer from extension: .toml -> .bt  on stdout
    bt.py --bt-to-toml -     # read .bt from stdin  -> TOML on stdout
    bt.py --toml-to-bt -     # read TOML from stdin -> .bt  on stdout

Always writes to stdout. `-` reads from stdin (direction must then be explicit).

Canonical form / round-trip rule:
    Behaviours are stored in TOML as a flat `[behaviours.<id>]` map. Anonymous
    inline behaviours are auto-named by tree path (`root`, `root.0`, `attack.1`, …).
    Going back to `.bt`, an id is **re-inlined** iff it is referenced exactly once
    and its id matches that path scheme; **user-given names** (and any behaviour
    referenced more than once) stay top-level named definitions referenced by name.
"""

import argparse
import os
import re
import sys
import tomllib

KINDS = {"Sequence", "Selector", "Join", "ForTicks", "Condition", "Action"}
COMPOSITES = {"Sequence", "Selector", "Join"}
_BARE = re.compile(r"[A-Za-z0-9_-]+")
_IDENT_START = re.compile(r"[A-Za-z_]")
_DOTTED = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*")


# ---------------------------------------------------------------------------
# Intermediate representation
#
#   doc = {
#     "run":  {"name": str, "seed": int},        # may be empty
#     "env":  {key: scalar},                      # scalar = int|float|str|bool
#     "behaviours": {id: node},                   # flat map
#     "root": id,                                 # entry behaviour id
#   }
#   node (by "type"):
#     Selector/Sequence/Join : {"type", "children": [id, ...]}
#     ForTicks               : {"type", "count": int, "child": id}
#     Condition              : {"type", "expression": str}
#     Action                 : {"type", "spec": {"kind": str, **params}}
# ---------------------------------------------------------------------------


class BtError(Exception):
    pass


# ---------------------------------------------------------------------------
# .bt parser (character-level recursive descent)
# ---------------------------------------------------------------------------
class Parser:
    def __init__(self, text):
        self.s = text
        self.i = 0
        self.n = len(text)
        self.behaviours = {}
        self.anon = 0

    def fail(self, msg):
        line = self.s.count("\n", 0, self.i) + 1
        col = self.i - (self.s.rfind("\n", 0, self.i))
        raise BtError(f"line {line}, col {col}: {msg}")

    def ws(self):
        while self.i < self.n:
            c = self.s[self.i]
            if c in " \t\r\n":
                self.i += 1
            elif c == "#":
                while self.i < self.n and self.s[self.i] != "\n":
                    self.i += 1
            else:
                break

    def peek(self):
        self.ws()
        return self.s[self.i] if self.i < self.n else ""

    def eat(self, ch):
        self.ws()
        if self.i >= self.n or self.s[self.i] != ch:
            self.fail(f"expected {ch!r}")
        self.i += 1

    def try_eat(self, ch):
        self.ws()
        if self.i < self.n and self.s[self.i] == ch:
            self.i += 1
            return True
        return False

    def ident(self):
        self.ws()
        m = _DOTTED.match(self.s, self.i)
        if not m:
            self.fail("expected identifier")
        self.i = m.end()
        return m.group()

    def string(self):
        self.eat('"')
        out = []
        while self.i < self.n:
            c = self.s[self.i]
            self.i += 1
            if c == "\\" and self.i < self.n:
                nxt = self.s[self.i]
                self.i += 1
                out.append({'"': '"', "\\": "\\", "n": "\n", "t": "\t"}.get(nxt, nxt))
            elif c == '"':
                return "".join(out)
            else:
                out.append(c)
        self.fail("unterminated string")

    def value(self):
        """A scalar: string | bool | float | int."""
        self.ws()
        c = self.peek()
        if c == '"':
            return self.string()
        m = re.match(r"-?\d+\.\d+(?:[eE][+-]?\d+)?|-?\d+[eE][+-]?\d+", self.s[self.i:])
        if m:
            self.i += m.end()
            return float(m.group())
        m = re.match(r"-?\d+", self.s[self.i:])
        if m:
            self.i += m.end()
            return int(m.group())
        m = re.match(r"true|false", self.s[self.i:])
        if m:
            self.i += m.end()
            return m.group() == "true"
        self.fail("expected a value (string/number/bool)")

    def balanced(self):
        """Capture raw text up to the matching ')' (for a Condition expression).

        Counts nested parens and skips string contents. Assumes the opening '('
        has already been consumed; consumes the closing ')'.
        """
        start = self.i
        depth = 1
        in_str = False
        while self.i < self.n:
            c = self.s[self.i]
            if in_str:
                if c == "\\":
                    self.i += 2
                    continue
                if c == '"':
                    in_str = False
            elif c == '"':
                in_str = True
            elif c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    text = self.s[start:self.i]
                    self.i += 1  # consume ')'
                    return text.strip()
            self.i += 1
        self.fail("unterminated '(' expression")

    # -- behaviours -------------------------------------------------------
    def opt_name(self):
        """Optional name after a kind keyword: a string, or a bare identifier
        that is not the opening bracket."""
        self.ws()
        c = self.peek()
        if c == '"':
            return self.string()
        if c and _IDENT_START.match(c):
            return self.ident()
        return None

    def behaviour(self, path):
        """Parse a behaviour; register it; return its id."""
        kind = self.ident()
        if kind not in KINDS:
            self.fail(f"unknown behaviour kind {kind!r}")
        name = self.opt_name()
        bid = name if name is not None else path
        if bid in self.behaviours:
            self.fail(f"duplicate behaviour id {bid!r}")

        if kind in COMPOSITES:
            self.eat("[")
            children = []
            if self.peek() != "]":
                while True:
                    children.append(self.child(f"{bid}.{len(children)}"))
                    if not self.try_eat(","):
                        break
            self.eat("]")
            self.behaviours[bid] = {"type": kind, "children": children}
        elif kind == "ForTicks":
            self.eat("(")
            count = self.value()
            if not isinstance(count, int):
                self.fail("ForTicks count must be an integer")
            self.eat(",")
            child = self.child(f"{bid}.0")
            self.eat(")")
            self.behaviours[bid] = {"type": kind, "count": count, "child": child}
        elif kind == "Condition":
            self.eat("(")
            expr = self.balanced()
            self.behaviours[bid] = {"type": kind, "expression": expr}
        elif kind == "Action":
            self.eat("(")
            action_id = self.string()
            spec = {"kind": action_id}
            while self.try_eat(","):
                key = self.ident()
                self.eat("=")
                spec[key] = self.value()
            self.eat(")")
            self.behaviours[bid] = {"type": kind, "spec": spec}
        return bid

    def child(self, path):
        """A child slot: a reference (bare name / string) or an inline behaviour."""
        self.ws()
        c = self.peek()
        if c == '"':
            return self.string()  # reference
        if c and _IDENT_START.match(c):
            save = self.i
            word = self.ident()
            if word in KINDS:
                self.i = save  # rewind; it's an inline behaviour
                return self.behaviour(path)
            return word  # bare reference
        self.fail("expected a child (reference or behaviour)")

    def block(self):
        """A `{ key = value ... }` block (run / env). Returns a dict."""
        self.eat("{")
        out = {}
        while self.peek() != "}":
            key = self.ident()
            self.eat("=")
            out[key] = self.value()
            self.try_eat(",")  # optional separator
        self.eat("}")
        return out

    def string_list(self):
        """A `[ "a", "b" ]` list of strings (include directive)."""
        self.eat("[")
        items = []
        if self.peek() != "]":
            while True:
                items.append(self.string())
                if not self.try_eat(","):
                    break
        self.eat("]")
        return items

    def document(self):
        doc = {"run": {}, "env": {}, "behaviours": self.behaviours,
               "root": None, "includes": []}
        while True:
            self.ws()
            if self.i >= self.n:
                break
            word_at = self.i
            word = self.ident()
            if word == "run":
                doc["run"] = self.block()
            elif word == "env":
                doc["env"] = self.block()
            elif word == "include":
                doc["includes"] += self.string_list()
            elif word == "root":
                self.eat("[")
                doc["root"] = self.child("root")
                self.eat("]")
            elif word in KINDS:
                self.i = word_at  # rewind to let behaviour() read the kind
                self.behaviour(f"_def{self.anon}")
                self.anon += 1
            else:
                self.fail(f"unexpected token {word!r}")
        # No required `root`: a document with run+root is a runnable attack; a
        # document with neither is a pure-behaviour library (fragment). The engine
        # enforces "an attack has run+root"; the translator is permissive.
        return doc


def parse_bt(text):
    return Parser(text).document()


# ---------------------------------------------------------------------------
# TOML reader (tomllib) -> IR
# ---------------------------------------------------------------------------
def _flatten(d, prefix=""):
    out = {}
    for k, v in d.items():
        key = f"{prefix}{k}"
        if isinstance(v, dict):
            out.update(_flatten(v, key + "."))
        else:
            out[key] = v
    return out


def parse_toml(text):
    data = tomllib.loads(text)
    run = dict(data.get("run", {}))
    root = run.pop("root", None)  # keep doc["run"] symmetric with the .bt parser
    doc = {
        "run": run,
        "env": _flatten(data.get("env", {})),
        "behaviours": {},
        "root": root,
        "includes": list(data.get("includes", [])),
    }
    for bid, node in data.get("behaviours", {}).items():
        n = dict(node)
        # The engine's dedicated honest leaf maps back to the canonical IR
        # form (an Action with kind "honest"), so it round-trips to .bt as
        # `Action("honest")`.
        if n.get("type") == "HonestAction":
            n = {"type": "Action", "spec": {"kind": "honest"}}
        doc["behaviours"][bid] = n
    return doc  # no root => a fragment (pure-behaviour library)


# ---------------------------------------------------------------------------
# TOML writer (hand-rolled)
# ---------------------------------------------------------------------------
def _toml_scalar(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, str):
        return '"' + v.replace("\\", "\\\\").replace('"', '\\"') + '"'
    raise BtError(f"unsupported scalar {v!r}")


def _toml_key(k):
    return k if _BARE.fullmatch(k) else '"' + k.replace('"', '\\"') + '"'


def write_toml(doc):
    out = []
    if doc.get("includes"):
        items = ", ".join(_toml_scalar(i) for i in doc["includes"])
        out.append(f"includes = [{items}]")

    if doc["run"] or doc["root"] is not None:
        if out:
            out.append("")
        out.append("[run]")
        run = dict(doc["run"])
        run["root"] = doc["root"]
        for k in ("name", "seed", "root"):
            if run.get(k) is not None:
                out.append(f"{k} = {_toml_scalar(run[k])}")
        for k, v in run.items():
            if k not in ("name", "seed", "root"):
                out.append(f"{_toml_key(k)} = {_toml_scalar(v)}")

    if doc["env"]:
        if out:
            out.append("")
        out.append("[env]")
        for k, v in doc["env"].items():
            out.append(f"{_toml_key(k)} = {_toml_scalar(v)}")

    for bid in sorted(doc["behaviours"]):
        node = doc["behaviours"][bid]
        if out:
            out.append("")
        out.append(f"[behaviours.{_toml_key(bid)}]")
        # The honest leaf is canonical in the IR as an Action with kind
        # "honest"; on the wire it is the engine's dedicated `HonestAction`
        # type (no spec).
        if node["type"] == "Action" and node["spec"].get("kind") == "honest":
            out.append('type = "HonestAction"')
            continue
        out.append(f'type = "{node["type"]}"')
        if node["type"] in COMPOSITES:
            kids = ", ".join(_toml_scalar(c) for c in node["children"])
            out.append(f"children = [{kids}]")
        elif node["type"] == "ForTicks":
            out.append(f"count = {node['count']}")
            out.append(f"child = {_toml_scalar(node['child'])}")
        elif node["type"] == "Condition":
            out.append(f"expression = {_toml_scalar(node['expression'])}")
        elif node["type"] == "Action":
            spec = node["spec"]
            parts = [f"kind = {_toml_scalar(spec['kind'])}"]
            parts += [f"{k} = {_toml_scalar(v)}" for k, v in spec.items() if k != "kind"]
            out.append("spec = { " + ", ".join(parts) + " }")
    return "\n".join(out) + "\n"


# ---------------------------------------------------------------------------
# .bt writer (hand-rolled), with re-inlining canonicalisation
# ---------------------------------------------------------------------------
def _classify(doc):
    """Return (refcount, parent) and an `inline(id)` predicate."""
    behaviours = doc["behaviours"]
    refcount = {bid: 0 for bid in behaviours}
    parent = {}

    def ref(child_id, parent_id, idx):
        if child_id in refcount:
            refcount[child_id] += 1
            parent[child_id] = (parent_id, idx)

    for bid, node in behaviours.items():
        if node["type"] in COMPOSITES:
            for k, c in enumerate(node["children"]):
                ref(c, bid, k)
        elif node["type"] == "ForTicks":
            ref(node["child"], bid, 0)
    ref(doc["root"], "__ROOT__", 0)

    def inline(bid):
        if refcount.get(bid, 0) != 1:
            return False
        p = parent.get(bid)
        if p is None:
            return False
        pid, idx = p
        if pid == "__ROOT__":
            return bid == "root"
        return bid == f"{pid}.{idx}"

    return inline


def _bt_value(v):
    return _toml_scalar(v)  # same scalar syntax


INDENT = "  "


def write_bt(doc):
    behaviours = doc["behaviours"]
    inline = _classify(doc)

    def emit_body(bid, ind):
        """The bracket/paren part after the kind keyword, indented at level `ind`."""
        node = behaviours[bid]
        t = node["type"]
        if t in COMPOSITES:
            if not node["children"]:
                return "[]"
            lines = [INDENT * (ind + 1) + emit_child(c, ind + 1) for c in node["children"]]
            return "[\n" + ",\n".join(lines) + "\n" + INDENT * ind + "]"
        if t == "ForTicks":
            cs = emit_child(node["child"], ind + 1)
            if "\n" in cs:
                return f"({node['count']},\n" + INDENT * (ind + 1) + cs + "\n" + INDENT * ind + ")"
            return f"({node['count']}, {cs})"
        if t == "Condition":
            return f"({node['expression']})"
        if t == "Action":
            spec = node["spec"]
            parts = [_bt_value(spec["kind"])]
            parts += [f"{k} = {_bt_value(v)}" for k, v in spec.items() if k != "kind"]
            return "(" + ", ".join(parts) + ")"
        raise BtError(f"unknown type {t!r}")

    def emit_child(bid, ind):
        """A child slot at level `ind`: inline behaviour or bare-name reference."""
        if bid in behaviours and inline(bid):
            return f"{behaviours[bid]['type']}{emit_body(bid, ind)}"
        return f'"{bid}"'  # reference

    out = []
    if doc.get("includes"):
        items = ", ".join(_bt_value(i) for i in doc["includes"])
        out.append(f"include [ {items} ]")
    if doc["run"]:
        if out:
            out.append("")
        out.append("run {")
        for k in ("name", "seed"):
            if k in doc["run"]:
                out.append(f"  {k} = {_bt_value(doc['run'][k])}")
        out.append("}")
    if doc["env"]:
        if out:
            out.append("")
        out.append("env {")
        for k, v in doc["env"].items():
            out.append(f"  {k} = {_bt_value(v)}")
        out.append("}")

    for bid in sorted(behaviours):
        if inline(bid):
            continue  # emitted inline at its single reference site
        node = behaviours[bid]
        if out:
            out.append("")
        out.append(f'{node["type"]} "{bid}" {emit_body(bid, 0)}')

    if doc["root"] is not None:
        if out:
            out.append("")
        root_id = doc["root"]
        rc = emit_child(root_id, 1) if inline(root_id) else f'"{root_id}"'
        if "\n" in rc:
            out.append("root [\n" + INDENT + rc + "\n]")
        else:
            out.append(f"root [ {rc} ]")
    return "\n".join(out) + "\n"


# ---------------------------------------------------------------------------
# Resolve includes -> one self-contained TOML (build step)
#
# Follows `include [...]` by bare name, searching the including file's directory plus
# any --include-path dirs (for `name`, looks for `name.bt`). Merges per the spec rules:
# env and behaviours deep-merge with closer-to-root winning; run/root come from the top
# (attack) document; includes are consumed (dropped from the output). The result is a
# self-contained TOML the engine can load without any include resolution.
# ---------------------------------------------------------------------------
def _read_file(path):
    return sys.stdin.read() if path == "-" else open(path, encoding="utf-8").read()


def _parse_any(path, text):
    if path != "-" and path.endswith(".toml"):
        return parse_toml(text)
    return parse_bt(text)


def _find_include(name, dirs):
    stem = name[:-3] if name.endswith(".bt") else name
    for d in dirs:
        cand = os.path.join(d, stem + ".bt")
        if os.path.isfile(cand):
            return cand
    raise BtError(f"include {name!r} not found (searched: {', '.join(dirs) or '.'})")


def resolve(path, include_paths, stack=()):
    """Recursively resolve `path`'s includes into one merged doc (includes consumed)."""
    if path in stack:
        raise BtError("include cycle: " + " -> ".join(stack + (path,)))
    doc = _parse_any(path, _read_file(path))
    dirs = [os.path.dirname(path) or "." if path != "-" else "."] + include_paths
    env, beh = {}, {}
    for inc in doc["includes"]:
        sub = resolve(_find_include(inc, dirs), include_paths, stack + (path,))
        env.update(sub["env"])          # later/closer-to-root overrides earlier
        beh.update(sub["behaviours"])
    env.update(doc["env"])              # the including (root) doc wins
    beh.update(doc["behaviours"])
    return {"run": doc["run"], "env": env, "behaviours": beh,
            "root": doc["root"], "includes": []}


def validate_refs(doc):
    """Every reference (root, children, ForTicks child) must resolve post-merge."""
    beh = doc["behaviours"]

    def check(bid, ctx):
        if bid not in beh:
            raise BtError(f"unresolved reference {bid!r} (from {ctx})")

    if doc["root"] is not None:
        check(doc["root"], "run.root")
    for bid, node in beh.items():
        if node["type"] in COMPOSITES:
            for c in node["children"]:
                check(c, f"behaviour {bid!r}")
        elif node["type"] == "ForTicks":
            check(node["child"], f"behaviour {bid!r}")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def main(argv=None):
    ap = argparse.ArgumentParser(description="Translate between .bt and TOML.")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--bt-to-toml", action="store_true", help="force .bt -> TOML")
    g.add_argument("--toml-to-bt", action="store_true", help="force TOML -> .bt")
    g.add_argument("--resolve", action="store_true",
                   help="resolve includes into one self-contained TOML (build step)")
    ap.add_argument("--include-path", action="append", default=[], metavar="DIR",
                    help="extra include search directory (repeatable)")
    ap.add_argument("file", help="input file, or - for stdin")
    args = ap.parse_args(argv)

    try:
        if args.resolve:
            doc = resolve(args.file, args.include_path)
            validate_refs(doc)
            sys.stdout.write(write_toml(doc))
            return 0

        direction = "bt2toml" if args.bt_to_toml else "toml2bt" if args.toml_to_bt else None
        if direction is None:
            if args.file.endswith(".bt"):
                direction = "bt2toml"
            elif args.file.endswith(".toml"):
                direction = "toml2bt"
            else:
                ap.error("cannot infer direction; pass --bt-to-toml or --toml-to-bt")

        text = _read_file(args.file)
        if direction == "bt2toml":
            sys.stdout.write(write_toml(parse_bt(text)))
        else:
            sys.stdout.write(write_bt(parse_toml(text)))
    except BtError as e:
        print(f"bt: error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
