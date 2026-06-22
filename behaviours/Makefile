BT_FILES = $(wildcard *.bt)

# Round-trip is *semantic* — comments and formatting are not preserved — so we don't
# diff the source against the round-trip. Instead we test idempotence per file: once a
# file has been through bt->toml->bt it must be stable (translating it again is a no-op).
#
# Make notes: `$(BT_FILES)` is a Make variable; `$$` is a literal `$` for the shell, so
# `$$$$` is the shell PID and `$$f`/`$$t`/`$$fail` are shell variables. The whole loop is
# one shell (joined with `\`) so the PID-based temp prefix is consistent throughout.
test:
	@fail=0; \
	for f in $(BT_FILES); do \
	  t=/tmp/bt-$$$$-$$(basename $$f .bt); \
	  ./bt.py $$f > $$t.toml && \
	  ./bt.py --toml-to-bt $$t.toml > $$t.bt && \
	  ./bt.py $$t.bt > $$t.toml2 && \
	  if diff -q $$t.toml $$t.toml2 >/dev/null; then \
	    echo "ok    $$f"; \
	  else \
	    echo "FAIL  $$f"; diff $$t.toml $$t.toml2; fail=1; \
	  fi; \
	  rm -f $$t.toml $$t.bt $$t.toml2; \
	done; \
	exit $$fail

.PHONY: test
