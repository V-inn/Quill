#!/bin/sh
# Waits for evdi's refcount to drop to 0 (i.e. KWin released card1 on logout),
# then unloads the old 1.14.8 module and loads the freshly-built 1.14.16 one.
# Gives up after ~2 minutes.
set -e

echo "watcher started, waiting for evdi refcount to reach 0..."
for i in $(seq 1 60); do
	USES=$(awk '$1=="evdi"{print $3}' /proc/modules)
	if [ -z "$USES" ]; then
		echo "evdi not loaded at all -- nothing to swap, exiting"
		exit 0
	fi
	if [ "$USES" = "0" ]; then
		echo "refcount is 0, swapping now"
		modprobe -r evdi
		modprobe evdi initial_device_count=1
		echo "swap done"
		/sbin/modinfo evdi | grep -E "filename|version"
		exit 0
	fi
	sleep 2
done
echo "timed out after 2 minutes, refcount never reached 0 (still: $USES)"
exit 1
