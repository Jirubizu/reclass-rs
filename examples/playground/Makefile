# Build the reclass-rs playground target.
#   make           # build ./playground
#   make run       # build and run it
#   make clean
#
# Built non-PIE (-no-pie) so &g_player is the same address every run, which
# makes the guide's addresses reproducible. Remove -no-pie for a PIE build if
# you'd rather practice the `<playground> + 0xRVA` module-relative workflow.

CC      ?= cc
CFLAGS  ?= -O0 -g -no-pie -fno-pie -Wall -Wextra

playground: playground.c
	$(CC) $(CFLAGS) -o $@ playground.c -lm

.PHONY: run clean
run: playground
	./playground

clean:
	rm -f playground
