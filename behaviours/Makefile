BT_FILE = long-range-fork.bt

# Round-trip is *semantic* — comments and formatting are not preserved — so we don't
# diff the source against the round-trip. Instead we test idempotence: once a file has
# been through bt->toml->bt it must be stable, i.e. translating it again is a no-op.

test:
	t=/tmp/bt-$$$$; \
	./bt.py $(BT_FILE)            > $$t.toml  && \
	./bt.py --toml-to-bt $$t.toml > $$t.bt    && \
	./bt.py $$t.bt               > $$t.toml2 && \
	diff $$t.toml $$t.toml2; \
	rc=$$?; rm -f $$t.toml $$t.bt $$t.toml2; \
	[ $$rc -eq 0 ] && echo "round-trip idempotent: OK"; \
	exit $$rc

.PHONY: test
