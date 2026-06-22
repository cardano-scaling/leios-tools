BT_FILES = $(wildcard lib/*.bt attacks/*.bt)

# Round-trip is *semantic* — comments and formatting are not preserved — so we don't
# diff the source against the round-trip. Instead we test idempotence per file: once a
# file has been through bt->toml->bt it must be stable (translating it again is a no-op).
# Covers both lib/ (pure behaviours/fragments) and attacks/ (runnable attacks).
#
# Make notes: `$(BT_FILES)` is a Make variable; `$$` is a literal `$` for the shell, so
# `$$$$` is the shell PID and `$$f`/`$$t`/`$$fail` are shell variables. The whole loop is
# one shell (joined with `\`) so the PID-based temp prefix is consistent throughout. The
# temp name sanitises the path (`lib/x.bt` -> `lib_x_bt`) so files in different dirs don't
# collide.
test:
	@fail=0; \
	for f in $(BT_FILES); do \
	  t=/tmp/bt-$$$$-$$(echo $$f | sed 's#[/.]#_#g'); \
	  if ./bt.py $$f > $$t.toml \
	     && ./bt.py --toml-to-bt $$t.toml > $$t.bt \
	     && ./bt.py $$t.bt > $$t.toml2 \
	     && diff -q $$t.toml $$t.toml2 >/dev/null; then \
	    echo "ok    $$f"; \
	  else \
	    echo "FAIL  $$f"; diff $$t.toml $$t.toml2 2>/dev/null || true; fail=1; \
	  fi; \
	  rm -f $$t.toml $$t.bt $$t.toml2; \
	done; \
	exit $$fail

# Full-resolution test: not a round trip (it merges an attack with its includes into one
# self-contained TOML), so we compare `--resolve` output against a committed golden file.
# Regenerate the golden with: make bless-resolve
RESOLVE_ATTACK = attacks/long-range-fork-attack.bt
RESOLVE_GOLDEN = expected/long-range-fork-attack.toml
test-resolve:
	@./bt.py --resolve --include-path lib $(RESOLVE_ATTACK) | diff - $(RESOLVE_GOLDEN) \
	  && echo "ok    resolve $(RESOLVE_ATTACK)" \
	  || { echo "FAIL  resolve $(RESOLVE_ATTACK) (differs from $(RESOLVE_GOLDEN); run 'make bless-resolve' if intended)"; exit 1; }

bless-resolve:
	@./bt.py --resolve --include-path lib $(RESOLVE_ATTACK) > $(RESOLVE_GOLDEN)
	@echo "blessed $(RESOLVE_GOLDEN)"

check: test test-resolve

.PHONY: test test-resolve bless-resolve check
