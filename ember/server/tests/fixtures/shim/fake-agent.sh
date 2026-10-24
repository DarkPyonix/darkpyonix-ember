#!/bin/sh
# Fake agent: prints "tick N" every 0.2 s for N in 1..$FAKE_TICKS (default 40), then exits 0.
# Lines read from stdin are echoed as "echo <line>".
# A background job in sh gets stdin from /dev/null, so keep the real stdin on fd 3.
exec 3<&0
(while read -r l <&3; do echo "echo $l"; done) &
i=1
while [ "$i" -le "${FAKE_TICKS:-40}" ]; do
  echo "tick $i"
  i=$((i + 1))
  sleep 0.2
done
