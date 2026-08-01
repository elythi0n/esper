#!/bin/sh
# A stand-in keeper for the tests: says the word the host waits for, then holds
# the pipe open until the host goes away.
#
# **A committed fixture rather than a file the tests write.** These tests run
# in parallel in one process, and a `fork` in one thread inherits every open
# file descriptor of every other thread — so a script another test had just
# written could still be open for writing inside a forked child that had not
# reached its own `exec`, and executing it failed with ETXTBSY about one run in
# four. Writing through a temporary and renaming does not help: the inode is
# the same one somebody has open. A file nobody writes cannot be busy.
echo keeping
cat > /dev/null
